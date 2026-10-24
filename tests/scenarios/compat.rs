use crate::support::World;

#[test]
fn a29_the_panel_and_observer_drive_the_daemon_unchanged() {
    let mut world = World::pair();
    world.close_case();
    let status = world.command("gram", "status");
    assert!(status.contains("\"schema_version\":1"));
    assert_eq!(world.command("gram", "activity").trim(), "accepted");
    assert_eq!(world.play("gram").trim(), "accepted");
    world.command("gram", "noise:anc");
    world.command("gram", "ear:both");
    world.run_for(12_000);
    world.invite("gram", "xps");
    world.run_for(12_000);
    world.submit("xps", "gram", "111111");
    world.run_for(12_000);
    assert!(world.status("xps")["fleet"]["code_attempt"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(world.phase("xps"), "incoming");
    assert!(world.status("gram")["schema_version"] == 1);
}

#[test]
fn a30_roster_and_route_run_both_directions() {
    // C++ enrollment is a JSON session inside BLE fragments, not these frames.
    // The roster and the route are the version 7 and version 5 advertisements
    // both daemons already exchange. Each laptop takes a turn as requester.
    use fleetpods::fleet::members::{Fleet, GroupKey, Member};
    use fleetpods::fleet::presence::FleetKeys;
    use fleetpods::fleet::roster_air::{self, Snapshot};
    use fleetpods::fleet::route::Phase;
    use fleetpods::fleet::route_air::{self, Answer, Kind};
    use fleetpods::panel::ipc::Command;
    use fleetpods::step::Input;
    use fleetpods::types::{Address, Clock, FleetId, UnixMs};

    let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
    let fleet = Fleet {
        id: FleetId::new([3; 16]),
        key: GroupKey::new([0x11; 32]),
        members: vec![
            Member { address: host(1), label: "gram".into() },
            Member { address: host(2), label: "xps".into() },
        ],
    };
    let pods = Address::parse("A0:00:00:00:00:03").unwrap();
    let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
    let now = Clock::new(1_000, 1_700_000_000_000);
    let aired = Clock::new(7_000, now.unix.0 + 6_000);

    for (requester_byte, helper_byte) in [(1u8, 2u8), (2, 1)] {
        // The requester needs a seat. An already connected host ignores activity.
        let mut requester = fleetpods::Daemon::new("requester");
        requester.set_local_address(Some(host(requester_byte)));
        requester.select_accessory(Some(pods));
        requester.restore_fleet(fleet.clone());
        let mut helper = seated(&fleet, pods, host(helper_byte), "helper");
        let (helper_presence, helper_session, helper_at) =
            aired_presence(&mut helper, &keys, host(requester_byte), aired);
        let (requester_presence, _, requester_at) =
            aired_presence(&mut requester, &keys, host(helper_byte), aired);
        let heard_at =
            if helper_at.mono.0 > requester_at.mono.0 { helper_at } else { requester_at };
        requester.step(Input::Advertisement(helper_presence), heard_at);
        helper.step(Input::Advertisement(requester_presence), heard_at);
        let roster = Snapshot {
            reporter: host(helper_byte),
            session: helper_session,
            sequence: 1,
            sent: heard_at.unix,
            accessory: pods,
            peers: vec![host(helper_byte), phone],
            // A playing phone keeps the pods from desktop activity; an idle one does not.
            audio_source: Some(phone),
            last_use: UnixMs(heard_at.unix.0 - 1),
            pending: false,
            audio_idle: true,
        };
        requester
            .step(Input::Advertisement(roster_air::advertise(&roster, &keys).unwrap()), heard_at);
        let learned = requester.status_value(heard_at);
        assert_eq!(learned["fleet"]["roster"]["peers"].as_array().unwrap().len(), 2);

        assert_eq!(
            requester.handle(Command::Activity, heard_at).body.as_deref(),
            Some("accepted\n")
        );
        let route_at = Clock::new(heard_at.mono.0 + 500, heard_at.unix.0 + 500);
        let asked = requester.step(Input::Tick, route_at);
        let request = route_air::read(advertised_route(&asked.effects), &keys).unwrap();
        assert_eq!(request.kind, Kind::Request);
        let reply_at = Clock::new(route_at.mono.0 + 500, route_at.unix.0 + 500);
        let reply = helper
            .step(Input::Advertisement(route_air::advertise(&request, &keys).unwrap()), reply_at);
        let admission = route_air::read(advertised_route(&reply.effects), &keys).unwrap();
        assert_eq!(admission.result, Answer::Ok);
        requester
            .step(Input::Advertisement(route_air::advertise(&admission, &keys).unwrap()), reply_at);
        assert_ne!(requester.routing().phase(), Phase::WaitingAdmission);
    }
}

fn aired_presence(
    daemon: &mut fleetpods::Daemon,
    keys: &fleetpods::fleet::presence::FleetKeys,
    listener: fleetpods::types::Address,
    at: fleetpods::types::Clock,
) -> (Vec<u8>, [u8; 16], fleetpods::types::Clock) {
    use fleetpods::fleet::presence;
    use fleetpods::step::{Effect, Input};

    let mut at = at;
    let mut bytes = None;
    // Public and private presence alternate at a three-second cadence.
    for _ in 0..20 {
        let stepped = daemon.step(Input::Tick, at);
        bytes = stepped.effects.iter().find_map(|effect| match effect {
            Effect::Advertise(frame) if frame.first() == Some(&presence::SEALED_SESSION) => {
                Some(frame.clone())
            }
            _ => None,
        });
        if bytes.is_some() {
            break;
        }
        at = fleetpods::types::Clock::new(at.mono.0 + 500, at.unix.0 + 500);
    }
    let bytes = bytes.expect("a sealed presence");
    let sight = presence::observe(&bytes, Some(listener), at.unix, Some(keys)).unwrap();
    (bytes, sight.session.expect("routing session"), at)
}

fn host(mark: u8) -> fleetpods::types::Address {
    fleetpods::types::Address::new([mark, 2, 3, 4, 5, 6]).unwrap()
}

fn seated(
    fleet: &fleetpods::fleet::members::Fleet,
    pods: fleetpods::types::Address,
    local: fleetpods::types::Address,
    name: &str,
) -> fleetpods::Daemon {
    use fleetpods::panel::ipc::Command;
    use fleetpods::step::Input;
    use fleetpods::types::Clock;

    let mut daemon = fleetpods::Daemon::new(name);
    let now = Clock::new(1_000, 1_700_000_000_000);
    daemon.set_local_address(Some(local));
    daemon.select_accessory(Some(pods));
    daemon.restore_fleet(fleet.clone());
    daemon.handle(Command::Connect, now);
    let generation = daemon.connection().generation();
    daemon.step(Input::ControlSocket { up: true, generation }, now);
    daemon.step(
        Input::Accessory { generation, packet: crate::support::accessory::handshake_ack() },
        now,
    );
    daemon.step(
        Input::Accessory { generation, packet: crate::support::accessory::features_ack() },
        now,
    );
    daemon.step(
        Input::Accessory { generation, packet: crate::support::accessory::sample_battery() },
        now,
    );
    daemon.step(
        Input::Accessory { generation, packet: crate::support::accessory::sample_ears() },
        now,
    );
    daemon
}

fn advertised_route(effects: &[fleetpods::step::Effect]) -> &[u8] {
    effects
        .iter()
        .find_map(|effect| match effect {
            fleetpods::step::Effect::Advertise(bytes)
                if bytes.first() == Some(&fleetpods::fleet::route_air::VERSION) =>
            {
                Some(bytes.as_slice())
            }
            _ => None,
        })
        .expect("a routing advertisement")
}

#[test]
fn a31_disagreement_keeps_provenance_and_one_snapshot() {
    // The gram's play takes the pods; the phone playing afterwards is the
    // newer report, and the gram's own claim does not mask it.
    let mut world = World::pair();
    world.open_case();
    world.play("gram");
    world.settle();
    world.remote_playing("iphone");
    world.settle();
    let status = world.status("gram");
    assert_eq!(status["reported_ownership"], 0, "direct report stays remote");
    assert_eq!(status["audio_source_local"], 0);
    assert_eq!(status["audio_owner"]["holder"], "remote");
    assert!(status["ownership_updated_ms"].as_u64().unwrap_or(0) > 0);
    assert!(status["source_updated_ms"].as_u64().unwrap_or(0) > 0);
    assert_eq!(world.holder("xps"), "remote");
    let again = world.status("gram");
    assert_eq!(again["audio_owner"], status["audio_owner"]);
    assert_eq!(again["reported_ownership"], status["reported_ownership"]);
}

#[test]
fn a32_only_the_current_operation_moves() {
    let mut world = World::single();
    world.open_case();
    world.command("gram", "connect");
    world.fail_connect("gram");
    world.settle();
    assert_eq!(world.stage("gram"), "retrying");
    world.command("gram", "handoff:release");
    world.settle();
    world.command("gram", "connect");
    world.fail_connect("gram");
    world.settle();
    assert_eq!(world.stage("gram"), "retrying");
    assert_ne!(world.stage("gram"), "failed");
    assert_eq!(world.connect_attempts("gram"), 1);
    let status = world.status("gram");
    let recovering = status["control_connection"]["recovering"] == true;
    let failed = status["control_connection"]["stage"] == "failed";
    assert!(recovering);
    assert!(!failed);
}
