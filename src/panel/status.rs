//! Compatibility JSON. This is a projection of one snapshot. Nothing reads it
//! back into a decision. Legacy sentinels (`-1`, `false`, omitted batteries) are
//! applied only here.
//!
//! Field sources are listed in `docs/status-fields.md`.

use serde_json::{json, Map, Value};

use crate::audio::mic::Microphone;
use crate::audio::queue::MicPath;
use crate::audio::seat::{Holder, Resolved};
use crate::fleet::enroll::Enrollment;
use crate::fleet::members::{Membership, Store};
use crate::fleet::nearby::Discovery;
use crate::fleet::roster::Peers;
use crate::fleet::route::Routing;
use crate::pods::link::{Connection, Stage};
use crate::pods::prefs::Config;
use crate::pods::reports::{Accessory, Placement, Pod};
use crate::types::{Battery, Clock, Evidence, LidState};

pub struct Snapshot<'a> {
    pub clock: Clock,
    pub local_name: &'a str,
    pub local_address: Option<crate::types::Address>,
    pub config: &'a Config,
    pub bluetooth_bonded: Option<bool>,
    pub connection: &'a Connection,
    pub accessory: &'a Accessory,
    pub discovery: &'a Discovery,
    pub peers: &'a Peers,
    pub membership: &'a Membership,
    pub enrollment: &'a Enrollment,
    pub routing: &'a Routing,
    pub microphone: &'a Microphone,
    pub mic_path: &'a MicPath,
    pub handoff_yielded: bool,
    pub handoff_phase: &'static str,
    pub handoff_failure: &'static str,
    pub handoff_updated: Option<crate::types::UnixMs>,
    pub audio_seen: bool,
    pub playing: bool,
    pub routed: bool,
    pub resolved: &'a Resolved,
    /// Fleet members that announced they are seated, besides the heard roster.
    pub seated: &'a [crate::types::Address],
    /// Last non-empty peer list from the other daemon, and who sent it.
    pub held_peers: &'a [crate::types::Address],
    pub held_reporter: Option<crate::types::Address>,
    /// Connected devices named by the pods, once a device list has arrived.
    pub pod_peers: &'a [crate::types::Address],
    pub pod_peers_known: bool,
    pub pod_peers_at: Option<crate::types::UnixMs>,
    pub displaced: bool,
    pub apple_labels: &'a std::collections::HashMap<crate::types::Address, String>,
}

pub fn project(snapshot: &Snapshot<'_>) -> Value {
    let connected = snapshot.connection.socket_up() == Some(true);
    let mut status = Map::new();
    status.insert("schema_version".into(), json!(1));
    insert_identity(&mut status, snapshot);
    insert_link(&mut status, snapshot, connected);
    insert_settings(&mut status, snapshot);
    insert_batteries(&mut status, snapshot.accessory);
    insert_presence(&mut status, snapshot);
    insert_ownership(&mut status, snapshot, connected);
    insert_fleet(&mut status, snapshot);
    insert_microphone(&mut status, snapshot);
    insert_handoff(&mut status, snapshot);
    Value::Object(status)
}

fn insert_handoff(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>) {
    let active = snapshot.audio_seen
        || snapshot.handoff_yielded
        || snapshot.handoff_phase != "idle"
        || !snapshot.handoff_failure.is_empty();
    if !active {
        return;
    }
    let mut handoff = Map::new();
    handoff.insert("enabled".into(), json!(true));
    handoff.insert("yielded".into(), json!(snapshot.handoff_yielded));
    handoff.insert("phase".into(), json!(snapshot.handoff_phase));
    handoff.insert("playing".into(), json!(snapshot.playing && !snapshot.handoff_yielded));
    handoff.insert("connecting".into(), json!(snapshot.handoff_phase == "reconnecting"));
    handoff.insert("attempting".into(), json!(snapshot.handoff_phase == "reconnecting"));
    handoff.insert("failure".into(), json!(snapshot.handoff_failure));
    handoff
        .insert("updated_ms".into(), json!(snapshot.handoff_updated.map(|at| at.0).unwrap_or(0)));
    let output = if snapshot.handoff_yielded {
        ""
    } else if snapshot.routed {
        "airpods"
    } else {
        ""
    };
    handoff.insert("audio_output".into(), json!(output));
    handoff.insert(
        "audio_active".into(),
        json!(snapshot.playing && snapshot.routed && !snapshot.handoff_yielded),
    );
    handoff.insert(
        "audio_state".into(),
        json!(if snapshot.playing && !snapshot.handoff_yielded {
            "playing"
        } else {
            "unavailable"
        }),
    );
    status.insert("native_handoff".into(), Value::Object(handoff));
}

pub fn line(snapshot: &Snapshot<'_>) -> String {
    let mut text = project(snapshot).to_string();
    text.push('\n');
    text
}

fn insert_microphone(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>) {
    use std::sync::atomic::Ordering;
    let microphone = snapshot.microphone;
    let mut object = Map::new();
    object.insert("phase".into(), json!(microphone.phase_wire()));
    object.insert("frames".into(), json!(microphone.frames()));
    object.insert("malformed".into(), json!(microphone.malformed()));
    object.insert("max_gap_ms".into(), json!(microphone.max_gap_ms()));
    object.insert(
        "decode_errors".into(),
        json!(snapshot.mic_path.decode_errors.load(Ordering::Relaxed)),
    );
    object.insert("dropped".into(), json!(snapshot.mic_path.dropped.load(Ordering::Relaxed)));
    object.insert("missing".into(), json!(snapshot.mic_path.missing.load(Ordering::Relaxed)));
    status.insert("microphone".into(), Value::Object(object));
}

fn insert_identity(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>) {
    let model = snapshot.accessory.model;
    status.insert("device_name".into(), json!(snapshot.accessory.name));
    status.insert("model_name".into(), json!(model.display_name()));
    status.insert("model_int".into(), json!(model.wire()));
    status.insert("model_number".into(), json!(snapshot.accessory.sku));
    status.insert("is_pro_series".into(), json!(model.is_pro_series()));
    status.insert("is_headset".into(), json!(model.is_headset()));
    status.insert("local_name".into(), json!(snapshot.local_name));
    status.insert(
        "local_address".into(),
        json!(snapshot.local_address.map(|address| address.display()).unwrap_or_default()),
    );
    status.insert("supports_noise_off".into(), json!(model.supports_noise_off()));
    status.insert("supports_noise_control".into(), json!(model.supports_noise_control()));
    status.insert("supports_adaptive".into(), json!(model.supports_adaptive()));
    status.insert(
        "supports_conversational_awareness".into(),
        json!(model.supports_conversation_awareness()),
    );
    status.insert("supports_one_bud_anc".into(), json!(model.supports_one_bud_anc()));
}

fn insert_link(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>, connected: bool) {
    let connection = snapshot.connection;
    status.insert("connected".into(), json!(connected));
    // The legacy bool has no unknown. Internal `None` stays `None` and is projected false.
    status.insert("bluez_connected".into(), json!(connection.bluez() == Some(true)));
    status.insert("ear_reported".into(), json!(snapshot.accessory.ear_reported));
    status.insert("pairing_keys_ready".into(), json!(snapshot.membership.accessory_keys_ready()));
    status.insert("bluetooth_paired".into(), json!(snapshot.config.accessory.is_some()));
    status.insert("bluetooth_bonded".into(), json!(snapshot.bluetooth_bonded));
    status.insert("handoff_ready".into(), json!(connected && connection.stage() == Stage::Ready));
    let (retry, limit) = connection.retry();
    let mut control = Map::new();
    control.insert("stage".into(), json!(published_stage(snapshot)));
    control.insert("reason".into(), json!(connection.reason()));
    control.insert("failed_stage".into(), json!(connection.failed_stage()));
    control.insert("recovering".into(), json!(connection.recovering()));
    control.insert("retry".into(), json!(retry));
    control.insert("retry_limit".into(), json!(limit));
    control.insert("peer_reconnect".into(), json!(connection.apple_reconnect()));
    control.insert("peer_reconnect_error".into(), json!(connection.peer_reconnect_error()));
    if let Some(deadline) = connection.deadline() {
        control.insert("next_retry_ms".into(), json!(snapshot.clock.unix_at(deadline).0));
    }
    status.insert("control_connection".into(), Value::Object(control));
}

fn insert_settings(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>) {
    let accessory = snapshot.accessory;
    let noise = accessory.noise.known().map(|mode| mode.wire()).unwrap_or(-1);
    status.insert("noise_mode".into(), json!(noise));
    if let Some(level) = accessory.adaptive.known() {
        status.insert("adaptive_noise_level".into(), json!(level));
    }
    // Confirmed booleans have no unknown on the wire. Off and not-yet-reported both read false.
    status.insert(
        "conversational_awareness".into(),
        json!(accessory.conversation.known() == Some(&true)),
    );
    status.insert("one_bud_anc_mode".into(), json!(accessory.one_bud.known() == Some(&true)));
    let ear = accessory.ear.known().copied().unwrap_or(snapshot.config.ear);
    status.insert("ear_detection_behavior".into(), json!(ear.wire()));
}

fn insert_batteries(status: &mut Map<String, Value>, accessory: &Accessory) {
    if let Some(pod) = pod_value(&accessory.left, &accessory.left_placement) {
        status.insert("left".into(), pod);
    }
    if let Some(pod) = pod_value(&accessory.right, &accessory.right_placement) {
        status.insert("right".into(), pod);
    }
    if let Some(battery) = accessory.case_battery.known() {
        status.insert("case".into(), battery_value(battery));
    }
    if let Some(battery) = accessory.headset.known() {
        status.insert("headset".into(), battery_value(battery));
    }
}

fn pod_value(pod: &Evidence<Pod>, placement: &Evidence<Placement>) -> Option<Value> {
    let battery = pod.known();
    let placement = placement
        .as_fresh()
        .copied()
        .or_else(|| battery.map(|pod| Placement { in_ear: pod.in_ear, in_case: pod.in_case }));
    if battery.is_none() && placement.is_none() {
        return None;
    }
    // A placement-only observation is available telemetry, not a battery reading.
    let mut object = battery
        .map(|pod| battery_map(&pod.battery))
        .unwrap_or_else(|| Map::from_iter([("available".into(), json!(true))]));
    if let Some(in_ear) = placement.and_then(|pod| pod.in_ear) {
        object.insert("in_ear".into(), json!(in_ear));
    }
    if let Some(in_case) = placement.and_then(|pod| pod.in_case) {
        object.insert("in_case".into(), json!(in_case));
    }
    Some(Value::Object(object))
}

fn battery_value(battery: &Battery) -> Value {
    Value::Object(battery_map(battery))
}

fn battery_map(battery: &Battery) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("available".into(), json!(true));
    object.insert("charging".into(), json!(battery.charging));
    object.insert("level".into(), json!(battery.level()));
    object
}

fn insert_presence(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>) {
    status.insert(
        "pairing_mode".into(),
        json!(
            snapshot.config.accessory.is_none()
                && snapshot.connection.bluez() != Some(true)
                && snapshot.accessory.pairing_at.is_some_and(|at| snapshot
                    .clock
                    .mono
                    .saturating_since(at)
                    < 3000)
        ),
    );
    let lid = &snapshot.accessory.lid;
    let live = match &lid.latest {
        Evidence::Fresh { value, .. } => value.wire(),
        _ => 2,
    };
    let last_known = lid.last_known.map(LidState::wire).unwrap_or(2);
    status.insert("lid_state".into(), json!(live));
    status.insert("last_known_lid_state".into(), json!(last_known));
    status.insert("lid_updated_ms".into(), json!(unix_at(snapshot.clock, lid.latest.at())));
    status.insert(
        "last_advertisement_ms".into(),
        json!(unix_at(snapshot.clock, snapshot.accessory.advertised_at)),
    );
    status.insert(
        "ble_connection_state".into(),
        json!(snapshot.accessory.ble_connection.unwrap_or(255)),
    );
    status.insert("case_confirmed_docked".into(), json!(snapshot.accessory.both_docked()));
    // Waiting for playback means this laptop was displaced and is holding for a
    // click or for audio. A prepared link, a closed case, and a failed route are not that.
    status.insert("waiting_for_playback".into(), json!(snapshot.displaced));
}

/// `waiting` on the wire means the case is shut or the buds are still docked.
/// Buds in the ear, an open lid, or a live call or music report are available,
/// so the panel must not say this laptop is waiting for the AirPods.
fn published_stage(snapshot: &Snapshot<'_>) -> &'static str {
    let stage = snapshot.connection.stage();
    if stage == Stage::Waiting && pods_available(snapshot) {
        "idle"
    } else {
        stage.wire()
    }
}

fn pods_available(snapshot: &Snapshot<'_>) -> bool {
    let worn = |pod: &Evidence<Pod>| pod.known().is_some_and(|pod| pod.in_ear == Some(true));
    if worn(&snapshot.accessory.left) || worn(&snapshot.accessory.right) {
        return true;
    }
    if matches!(snapshot.accessory.lid.latest, Evidence::Fresh { value: LidState::Open, .. }) {
        return true;
    }
    let in_use = matches!(snapshot.accessory.ble_connection, Some(4 | 5 | 6 | 7 | 9));
    let fresh = snapshot
        .accessory
        .advertised_at
        .is_some_and(|at| snapshot.clock.mono.saturating_since(at) < 20_000);
    in_use && fresh
}

fn insert_ownership(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>, connected: bool) {
    let accessory = snapshot.accessory;
    status.insert("reported_ownership".into(), json!(bit(accessory.ownership.known().copied())));
    status.insert(
        "audio_source_local".into(),
        json!(bit(accessory.source.known().map(|source| source.local))),
    );
    status.insert(
        "audio_source_type".into(),
        json!(accessory.source.known().map(|source| i64::from(source.kind)).unwrap_or(-1)),
    );
    let (holder, playing) = match &snapshot.resolved.holder {
        Holder::Local { playing } => ("local", *playing),
        Holder::Remote { playing, .. } => ("remote", *playing),
        Holder::Unknown => ("", false),
    };
    status.insert("audio_owner".into(), json!({ "holder": holder, "playing": playing }));
    status.insert(
        "remote_playing".into(),
        json!(matches!(snapshot.resolved.holder, Holder::Remote { playing: true, .. })),
    );
    let roster =
        snapshot.config.accessory.and_then(|address| snapshot.peers.fresh(snapshot.clock, address));
    let owner_address = match snapshot.resolved.holder {
        Holder::Local { .. } => snapshot.local_address,
        Holder::Remote { address: Some(address), .. } => Some(address),
        _ => roster.and_then(|report| report.audio_source),
    };
    let roster_owner = owner_address.map(|address| address.display());
    status.insert("roster_audio_owner".into(), json!(roster_owner.unwrap_or_default()));
    if let Some(address) = accessory.remote_address.known() {
        status.insert("remote_source_address".into(), json!(address.display()));
    }
    status.insert(
        "ownership_updated_ms".into(),
        json!(unix_at(snapshot.clock, accessory.ownership.at())),
    );
    status
        .insert("source_updated_ms".into(), json!(unix_at(snapshot.clock, accessory.source.at())));
    let pod_list = connected.then(|| pod_broadcast(snapshot)).flatten();
    // The panel reads a time beside `connected` as the pods' own list. An open socket still in setup has none,
    // and the helper's roster time would replace the seats it remembers with an empty list.
    let peer_list_at = match &pod_list {
        Some(_) => snapshot.pod_peers_at.map(|at| at.0).unwrap_or(0),
        None if connected => 0,
        None => roster.map(|report| report.sent.0).unwrap_or(0),
    };
    status.insert("peer_list_updated_ms".into(), json!(peer_list_at));
    status.insert("ownership_intent".into(), json!(snapshot.routing.intent().wire()));
    status.insert("handoff_result".into(), json!(snapshot.routing.handoff_result()));
    let peer_count = if let Some(listed) = &pod_list {
        listed.len() as i64
    } else if connected {
        roster.map(|report| report.peers.len() as i64).unwrap_or(-1)
    } else {
        -1
    };
    status.insert("remote_peer_count".into(), json!(peer_count));
    let fresh: Vec<crate::types::Address> = if connected {
        roster.map(|report| report.peers.clone()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let direct_peers = pod_list.is_some();
    let mut peers: Vec<Value> = if let Some(listed) = pod_list {
        listed.iter().map(|address| json!(address.display())).collect()
    } else if !fresh.is_empty() {
        fresh.iter().map(|address| json!(address.display())).collect()
    } else if connected && reporter_still_nearby(snapshot) {
        snapshot.held_peers.iter().map(|address| json!(address.display())).collect()
    } else {
        Vec::new()
    };
    // A playback announcement can name only the source. Laptops that are still
    // seated stay on the list, so one of them does not drop into the tray.
    // The pods' own list already names everyone, so it is not filled in.
    if connected && !snapshot.pod_peers_known {
        for address in snapshot.seated {
            if peers.len() >= 2 {
                break;
            }
            let text = address.display();
            if peers.iter().any(|value| value.as_str() == Some(text.as_str())) {
                continue;
            }
            if snapshot.membership.contains(*address) {
                peers.push(json!(text));
            }
        }
    }
    // Admission supersedes the helper's old roster while our socket opens.
    // A fresh AirPods list may still confirm that the helper really is present.
    if !direct_peers
        && matches!(
            snapshot.routing.phase(),
            crate::fleet::route::Phase::Connecting | crate::fleet::route::Phase::WaitingCompletion
        )
    {
        if let (Some(helper), Some(retained)) = (snapshot.routing.peer(), snapshot.routing.retain())
        {
            if helper != retained {
                let helper = helper.display();
                peers.retain(|peer| peer.as_str() != Some(helper.as_str()));
            }
        }
    }
    status.insert("connected_peers".into(), Value::Array(peers));
    status.insert("want_omarchy_seat".into(), json!(snapshot.routing.want_seat()));
    status.insert("holding_omarchy_seat".into(), json!(snapshot.routing.want_seat() && !connected));
    let devices = devices(snapshot);
    if !devices.is_empty() {
        status.insert("devices".into(), Value::Array(devices));
    }
}

fn insert_fleet(status: &mut Map<String, Value>, snapshot: &Snapshot<'_>) {
    let enrollment = snapshot.enrollment;
    let (peer_address, peer_name) = match enrollment.peer() {
        Some((address, name)) => (address.display(), name.to_string()),
        None => (String::new(), String::new()),
    };
    let mut fleet = Map::new();
    fleet.insert("phase".into(), json!(enrollment.phase().wire()));
    fleet.insert("id".into(), json!(enrollment.id().map(|id| id.hex()).unwrap_or_default()));
    fleet.insert("peer_address".into(), json!(peer_address));
    fleet.insert("peer_name".into(), json!(peer_name));
    fleet.insert("initiator".into(), json!(enrollment.initiator()));
    fleet.insert(
        "expires_ms".into(),
        json!(enrollment.expiry().map(|at| snapshot.clock.unix_at(at).0).unwrap_or(0)),
    );
    fleet.insert("error".into(), json!(enrollment.error()));
    fleet.insert("code_attempt".into(), json!(enrollment.code_attempt()));
    fleet.insert("enrolled".into(), json!(matches!(snapshot.membership.store(), Store::Open(_))));
    fleet.insert(
        "waiting_for_keyring".into(),
        json!(matches!(snapshot.membership.store(), Store::Unavailable)),
    );
    if let Some(code) = enrollment.displayed_code() {
        fleet.insert("code".into(), json!(code));
    }
    // The fleet transport is up only while discovery and the advertisement are both registered.
    fleet.insert("available".into(), json!(false));
    fleet.insert(
        "routing".into(),
        json!({
            "phase": snapshot.routing.phase().wire(),
            "busy": snapshot.routing.phase().busy(),
            "admission": snapshot.routing.admission(),
            "error": snapshot.routing.error(),
            "peer": snapshot.routing.peer().map(|address| address.display()).unwrap_or_default(),
            "retain": snapshot.routing.retain().map(|address| address.display()).unwrap_or_default(),
        }),
    );
    fleet.insert("roster".into(), roster_value(snapshot));
    status.insert("fleet".into(), Value::Object(fleet));
    if let Some(outcome) = snapshot.routing.outcome() {
        if outcome.at.0 > 0 {
            status.insert(
                "fleet_outcome".into(),
                json!({
                    "at_ms": outcome.at.0,
                    "result": outcome.result,
                    "peer": outcome.peer.display(),
                    "retain": outcome.retain.map(|address| address.display()).unwrap_or_default(),
                }),
            );
        }
    }
}

/// The pods' connected devices while this laptop is one of at most two.
/// `None` until a device list arrives. An unusable list is empty.
fn pod_broadcast(snapshot: &Snapshot<'_>) -> Option<Vec<crate::types::Address>> {
    if !snapshot.pod_peers_known || snapshot.connection.stage() != crate::pods::link::Stage::Ready {
        return None;
    }
    let Some(local) = snapshot.local_address else {
        return Some(Vec::new());
    };
    if snapshot.pod_peers.contains(&local) && snapshot.pod_peers.len() <= 2 {
        // As in the daemon's broadcast: a solo list from a simultaneous
        // connection is completed by the other laptop's fresh roster.
        let report = snapshot.config.accessory.zip(snapshot.held_reporter).and_then(
            |(accessory, reporter)| {
                snapshot
                    .peers
                    .fresh_from(snapshot.clock, accessory, reporter)
                    .map(|report| report.peers.as_slice())
            },
        );
        Some(crate::fleet::roster::complete_solo_list(
            snapshot.pod_peers,
            local,
            snapshot.held_reporter,
            report.unwrap_or(&[]),
        ))
    } else {
        Some(Vec::new())
    }
}

fn reporter_still_nearby(snapshot: &Snapshot<'_>) -> bool {
    let Some(reporter) = snapshot.held_reporter else {
        return false;
    };
    snapshot.discovery.hosts().iter().any(|host| host.address == reporter && host.authenticated)
}

fn roster_value(snapshot: &Snapshot<'_>) -> Value {
    let Some(accessory) = snapshot.config.accessory else {
        return json!({});
    };
    let Some(report) = snapshot.peers.fresh(snapshot.clock, accessory) else {
        return json!({});
    };
    json!({
        "reporter": report.reporter_address.display(),
        "accessory": report.accessory.display(),
        "peers": report.peers.iter().map(|address| address.display()).collect::<Vec<_>>(),
        "audio_source": report.audio_source.map(|address| address.display()).unwrap_or_default(),
        "sent_ms": report.sent.0,
        "pending": report.pending,
        "audio_idle": report.audio_idle,
    })
}

fn devices(snapshot: &Snapshot<'_>) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Store::Open(fleet) = snapshot.membership.store() {
        for member in &fleet.members {
            let presence =
                snapshot.discovery.hosts().iter().find(|host| host.address == member.address);
            // A peer's reset does not erase our saved membership. Its fresh
            // public announcement can still offer enrollment in the tray.
            let unjoined =
                presence.is_some_and(|host| !host.authenticated && host.enrolled == Some(false));
            let mut row = json!({
                "address": member.address.display(),
                "label": member.label,
                "kind": "omarchy",
                "discovered": unjoined,
                "nearby": unjoined || presence.is_some_and(|host| host.authenticated),
            });
            if unjoined {
                row["fleet_enrolled"] = json!(false);
            }
            rows.push(row);
        }
    }
    for nearby in snapshot.discovery.hosts() {
        let address = nearby.address.display();
        if rows
            .iter()
            .any(|row| row.get("address").and_then(Value::as_str) == Some(address.as_str()))
        {
            continue;
        }
        rows.push(json!({
            "address": address,
            "label": nearby.name,
            "fleet_enrolled": nearby.enrolled,
            "kind": "omarchy",
            "discovered": true,
        }));
    }
    // Saved devices belong in the tray even when they are absent from the
    // current connection roster. Connected peers remain a separate field.
    for (address, label) in snapshot.apple_labels {
        if Some(*address) == snapshot.local_address || Some(*address) == snapshot.config.accessory {
            continue;
        }
        let text = address.display();
        if rows.iter().any(|row| row.get("address").and_then(Value::as_str) == Some(text.as_str()))
        {
            continue;
        }
        rows.push(json!({
            "address": text,
            "label": label,
            "kind": "apple",
            "discovered": false,
            "nearby": false,
        }));
    }
    rows.sort_by(|left, right| {
        left["address"].as_str().unwrap_or("").cmp(right["address"].as_str().unwrap_or(""))
    });
    rows
}

fn bit(value: Option<bool>) -> i64 {
    match value {
        Some(true) => 1,
        Some(false) => 0,
        None => -1,
    }
}

fn unix_at(clock: Clock, at: Option<crate::types::MonoMs>) -> u64 {
    at.map(|at| clock.unix_at(at).0).unwrap_or(0)
}
