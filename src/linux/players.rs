//! MPRIS players on the session bus.
//!
//! The startup picture is one input. Later changes arrive as signals. This
//! thread does not run on the session loop and does not touch the radio.

use std::collections::HashMap;
use std::time::Duration;

use dbus::arg::{RefArg, Variant};
use dbus::blocking::Connection;
use dbus::message::Message;

use crate::audio::play::PlayerSeen;
use crate::linux::wake::Wake;
use crate::step::Input;

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

enum Job {
    Stop,
    Pause(Vec<String>),
}

pub struct Handle {
    jobs: Sender<Job>,
    wake: Wake,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn pause(&self, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        let _ = self.jobs.send(Job::Pause(names));
        self.wake.poke();
    }

    pub fn stop(&mut self) {
        let _ = self.jobs.send(Job::Stop);
        self.wake.poke();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Known {
    service: String,
    playing: bool,
}

pub fn spawn(reports: Sender<Input>) -> Option<Handle> {
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let (wake, thread_wake) = Wake::pair().ok()?;
    let thread = std::thread::Builder::new()
        .name("fleetpods-mpris".into())
        .spawn(move || run(jobs_rx, reports, thread_wake))
        .ok()?;
    Some(Handle { jobs: jobs_tx, wake, thread: Some(thread) })
}

fn run(jobs: Receiver<Job>, reports: Sender<Input>, wake: Wake) {
    let Ok(bus) = crate::linux::bluez::open_bus(dbus::channel::BusType::Session) else {
        return;
    };
    if listen(&bus).is_err() {
        return;
    }
    let mut players = HashMap::new();
    if let Some(startup) = startup(&bus, &mut players) {
        let _ = reports.send(Input::Players(startup));
    }
    let fd = bus.channel().watch().fd;
    loop {
        if !drain_jobs(&jobs, &bus) {
            break;
        }
        let mut waiting = [
            libc::pollfd { fd: wake.raw(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
        ];
        let _ = unsafe { libc::poll(waiting.as_mut_ptr(), waiting.len() as libc::nfds_t, -1) };
        wake.drain();
        if !drain_jobs(&jobs, &bus) {
            break;
        }
        if waiting[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
            continue;
        }
        if bus.channel().read_write(Some(Duration::ZERO)).is_err() {
            break;
        }
        while let Some(message) = bus.channel().pop_message() {
            apply(&bus, &reports, &mut players, message);
        }
    }
}

fn drain_jobs(jobs: &Receiver<Job>, bus: &Connection) -> bool {
    loop {
        match jobs.try_recv() {
            Ok(Job::Stop) | Err(mpsc::TryRecvError::Disconnected) => return false,
            Ok(Job::Pause(names)) => {
                for name in names {
                    pause_player(bus, &name);
                }
            }
            Err(mpsc::TryRecvError::Empty) => return true,
        }
    }
}

fn pause_player(bus: &Connection, service: &str) {
    let proxy = bus.with_proxy(service, "/org/mpris/MediaPlayer2", Duration::from_millis(200));
    eprintln!("fleetpodsd: media command source=fleetpods player={service:?} method=Pause sent");
    let called: Result<(), dbus::Error> =
        proxy.method_call("org.mpris.MediaPlayer2.Player", "Pause", ());
    match called {
        Ok(()) => eprintln!("fleetpodsd: media command player={service:?} method=Pause accepted"),
        Err(err) => {
            eprintln!("fleetpodsd: media command player={service:?} method=Pause failed: {err}")
        }
    }
}

fn listen(bus: &Connection) -> Result<(), String> {
    bus.add_match_no_cb("type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged'")
        .map_err(|err| err.to_string())?;
    bus.add_match_no_cb(
        "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged'",
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn startup(bus: &Connection, players: &mut HashMap<String, Known>) -> Option<Vec<PlayerSeen>> {
    let proxy =
        bus.with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", Duration::from_millis(200));
    let (names,): (Vec<String>,) =
        proxy.method_call("org.freedesktop.DBus", "ListNames", ()).ok()?;
    let mut seen = Vec::new();
    for name in names {
        if !name.starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }
        let Some(owner) = name_owner(bus, &name) else {
            continue;
        };
        let Some(playing) = status(bus, &name) else {
            continue;
        };
        eprintln!("fleetpodsd: media snapshot source=mpris player={name:?} playing={playing}");
        players.insert(owner, Known { service: name.clone(), playing });
        seen.push(PlayerSeen { service: name, playing });
    }
    seen.sort_by(|left, right| left.service.cmp(&right.service));
    Some(seen)
}

fn apply(
    bus: &Connection,
    reports: &Sender<Input>,
    players: &mut HashMap<String, Known>,
    message: Message,
) {
    let interface = message.interface().map(|value| value.to_string()).unwrap_or_default();
    let member = message.member().map(|value| value.to_string()).unwrap_or_default();
    if interface == "org.freedesktop.DBus" && member == "NameOwnerChanged" {
        let Ok((name, _old, new)) = message.read3::<String, String, String>() else {
            return;
        };
        if !name.starts_with("org.mpris.MediaPlayer2.") {
            return;
        }
        if new.is_empty() {
            let gone = players
                .iter()
                .find(|(_, known)| known.service == name)
                .map(|(owner, _)| owner.clone());
            if let Some(owner) = gone {
                players.remove(&owner);
                eprintln!("fleetpodsd: media event source=mpris player={name:?} removed");
                let _ = reports.send(Input::Player { service: name, playing: None });
            }
            return;
        }
        let Some(playing) = status(bus, &name) else {
            return;
        };
        eprintln!("fleetpodsd: media snapshot source=mpris player={name:?} playing={playing}");
        players.insert(new, Known { service: name.clone(), playing });
        let _ = reports.send(Input::Player { service: name, playing: Some(playing) });
        return;
    }
    if interface != "org.freedesktop.DBus.Properties" || member != "PropertiesChanged" {
        return;
    }
    let Some(sender) = message.sender().map(|sender| sender.to_string()) else {
        return;
    };
    let Some(known) = players.get(&sender) else {
        return;
    };
    let Ok((iface, changed)) = message.read2::<String, HashMap<String, Variant<Box<dyn RefArg>>>>()
    else {
        return;
    };
    if iface != "org.mpris.MediaPlayer2.Player" {
        return;
    }
    if let Some(volume) = changed.get("Volume").and_then(|value| value.0.as_f64()) {
        eprintln!(
            "fleetpodsd: media event source=mpris player={:?} volume={volume}",
            known.service
        );
    }
    let Some(playing) = changed.get("PlaybackStatus").and_then(|value| value.0.as_str()) else {
        return;
    };
    // Log the reported state before the policy's bool deduplication. Paused and
    // Stopped both mean !playing there, but are different diagnostic evidence.
    eprintln!(
        "fleetpodsd: media event source=mpris player={:?} playback={playing:?} previous_playing={}",
        known.service, known.playing
    );
    let playing = playing == "Playing";
    if known.playing == playing {
        return;
    }
    let service = known.service.clone();
    if let Some(known) = players.get_mut(&sender) {
        known.playing = playing;
    }
    let _ = reports.send(Input::Player { service, playing: Some(playing) });
}

fn name_owner(bus: &Connection, name: &str) -> Option<String> {
    let proxy =
        bus.with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", Duration::from_millis(80));
    let (owner,): (String,) =
        proxy.method_call("org.freedesktop.DBus", "GetNameOwner", (name,)).ok()?;
    Some(owner)
}

fn status(bus: &Connection, name: &str) -> Option<bool> {
    let proxy = bus.with_proxy(name, "/org/mpris/MediaPlayer2", Duration::from_millis(80));
    let (value,): (Variant<String>,) = proxy
        .method_call(
            "org.freedesktop.DBus.Properties",
            "Get",
            ("org.mpris.MediaPlayer2.Player", "PlaybackStatus"),
        )
        .ok()?;
    Some(value.0 == "Playing")
}
