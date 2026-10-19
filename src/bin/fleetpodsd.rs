//! Headless FleetPods daemon. BlueZ, the control socket, the keyring, MPRIS,
//! and PipeWire are watched. A letter is advertised only while it is queued.

use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fleetpods::linux::os;
use fleetpods::linux::paths::{self, SessionPaths};
use fleetpods::linux::session::Session;
use fleetpods::panel::ipc::{self, Incoming};
use fleetpods::types::Clock;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("Usage: fleetpodsd\n\nServe the FleetPods status file and command socket.");
        return;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("fleetpodsd {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let paths = match session_paths() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("fleetpodsd: {err}");
            std::process::exit(1);
        }
    };
    let listener = match bind_socket(&paths.socket) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("fleetpodsd: {err}");
            std::process::exit(1);
        }
    };

    os::install_stop_signals();
    let boot = Boot::now();
    let mut session = Session::start(os::hostname());
    let watermark = paths.status.with_file_name("roster.watermark");
    session.use_roster_file(&watermark);
    let mut published = String::new();
    if let Err(err) = publish(&paths.status, &session.status(boot.clock()), &mut published) {
        eprintln!("fleetpodsd: {err}");
        let _ = fs::remove_file(&paths.socket);
        std::process::exit(1);
    }
    eprintln!("fleetpodsd: watching BlueZ, the accessory, the keyring, and PipeWire");

    while !os::stop_requested() {
        session.poll(boot.clock());
        if let Err(err) = publish(&paths.status, &session.status(boot.clock()), &mut published) {
            eprintln!("fleetpodsd: {err}");
        }
        match os::poll_in_or_wake(listener.as_raw_fd(), session.wake_fd(), 200) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(err) => {
                eprintln!("fleetpodsd: {err}");
                break;
            }
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
            Err(err) => {
                eprintln!("fleetpodsd: {err}");
                continue;
            }
        };
        if !same_user(&stream) {
            eprintln!("fleetpodsd: command socket connection from another user refused");
            continue;
        }
        if let Some(command) = read_command(&mut stream) {
            if let Some(body) = session.command(command, boot.clock()) {
                let _ = stream.write_all(body.as_bytes());
            }
        }
        if let Err(err) = publish(&paths.status, &session.status(boot.clock()), &mut published) {
            eprintln!("fleetpodsd: {err}");
        }
    }

    session.shutdown();
    let _ = fs::remove_file(&paths.status);
    let _ = fs::remove_file(&paths.socket);
}

struct Boot {
    started: Instant,
}

impl Boot {
    fn now() -> Self {
        Self { started: Instant::now() }
    }

    /// `Instant` stops while the machine sleeps, so wall time is read fresh
    /// rather than derived from it. Otherwise every published timestamp and
    /// every cross-host freshness check would lag by the time spent asleep.
    fn clock(&self) -> Clock {
        let mono = self.started.elapsed().as_millis() as u64;
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        Clock::new(mono, unix)
    }
}

fn session_paths() -> Result<SessionPaths, String> {
    let runtime = env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty());
    let Some(runtime) = runtime else {
        return Err(paths::PathError::RuntimeMissing.to_string());
    };
    let runtime = PathBuf::from(runtime);
    let meta = fs::metadata(&runtime).map_err(|_| paths::PathError::RuntimeMissing.to_string())?;
    let state = state_home().ok_or_else(|| paths::PathError::StateMissing.to_string())?;
    paths::resolve(&runtime, &meta, &state).map_err(|err| err.to_string())
}

fn state_home() -> Option<PathBuf> {
    if let Some(state) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(state));
    }
    let home = env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".local/state"))
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("another daemon already owns {}", paths::SOCKET_NAME),
                ));
            }
            Err(_) => {
                let _ = fs::remove_file(path);
            }
        }
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn publish(path: &Path, body: &str, previous: &mut String) -> io::Result<()> {
    if body == previous {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    *previous = body.to_string();
    Ok(())
}

/// The socket lives in a per-user runtime directory and is mode 0600, but
/// peer credentials make the boundary explicit rather than a consequence
/// of directory permissions: only this user's own processes may command
/// the daemon (fleet reset, AirPods forget, seat claims).
fn same_user(stream: &UnixStream) -> bool {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut cred).cast(),
            &mut len,
        )
    };
    rc == 0 && cred.uid == unsafe { libc::geteuid() }
}

fn read_command(stream: &mut UnixStream) -> Option<fleetpods::panel::ipc::Command> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        if buf.len() > ipc::MAX_BYTES {
            return None;
        }
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(_) => return None,
        }
        match ipc::push(&buf) {
            Incoming::NeedMore => continue,
            Incoming::TooLarge => return None,
            Incoming::Ready(command) => return Some(command),
        }
    }
    match ipc::push(&buf) {
        Incoming::Ready(command) => Some(command),
        _ => None,
    }
}
