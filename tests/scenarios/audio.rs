use fleetpods::audio::play::StreamSnap;

use crate::support::{profile, Slot, World};

fn stereo_catalog() -> Vec<fleetpods::audio::play::Profile> {
    vec![
        profile("a2dp-sink-aac", "High Fidelity Playback (A2DP Sink, codec AAC)", 80, 0),
        profile("a2dp-sink-sbc_xq", "High Fidelity Playback (A2DP Sink, codec SBC-XQ)", 10, 0),
        profile("headset-head-unit-msbc", "Headset Head Unit (HSP/HFP, codec mSBC)", 100, 1),
    ]
}

#[test]
fn a21_activation_reselects_the_highest_ranked_stereo_profile() {
    let mut world = World::single();
    world.offer_profiles("gram", stereo_catalog());
    world.activate_audio("gram");
    world.settle();
    assert_eq!(world.selected_profile("gram").as_deref(), Some("a2dp-sink-sbc_xq"));
    world.restart("gram");
    world.offer_profiles("gram", stereo_catalog());
    world.activate_audio("gram");
    world.settle();
    assert_eq!(world.selected_profile("gram").as_deref(), Some("a2dp-sink-sbc_xq"));
}

#[test]
fn a22_a_reused_stream_id_does_not_restore_the_wrong_stream() {
    let mut world = World::single();
    world.open_case();
    world.play("gram");
    world.hold_streams("gram", vec![StreamSnap { id: 4, generation: 1, mute: false, volume: 1.4 }]);
    world.fail_connect("gram");
    world.settle();
    world.hold_streams("gram", vec![StreamSnap { id: 4, generation: 2, mute: true, volume: 0.2 }]);
    world.settle();
    let restored = world.restorations("gram");
    assert!(
        restored
            .iter()
            .any(|stream| stream.id == 4 && stream.generation == 1 && stream.volume == 1.4),
        "the original stream should be restored"
    );
    assert!(
        !restored.iter().any(|stream| stream.generation == 2),
        "a reused stream id must not take the held gain"
    );
}

#[test]
fn a23_ear_removal_and_conversation_awareness_restore_volume() {
    let mut world = World::single();
    world.open_case();
    world.play("gram");
    world.command("gram", "ca:on");
    world.settle();
    world.ears("gram", Some(false), Some(true));
    world.advance(500);
    world.settle();
    assert_eq!(world.status("gram")["ownership_intent"], "claim");
    assert_ne!(world.holder("gram"), "remote");
    world.ears("gram", Some(true), Some(true));
    world.advance(500);
    world.settle();
    assert_eq!(world.holder("gram"), "local");
    world.remote_playing("iphone");
    world.ears("gram", Some(false), Some(false));
    world.advance(500);
    world.ears("gram", Some(true), Some(true));
    world.advance(500);
    world.settle();
    assert_eq!(world.holder("gram"), "remote", "reinsertion must not steal the phone");
}

#[test]
fn a24_recording_uses_the_airpods_input_and_keeps_stereo() {
    let mut world = World::single();
    world.open_case();
    world.offer_profiles("gram", stereo_catalog());
    world.play("gram");
    world.mic("gram", 1, false);
    world.tone("gram", 1_000);
    world.settle();
    assert!(world.mic_samples("gram") > 0, "capture should deliver samples");
    assert_eq!(world.selected_profile("gram").as_deref(), Some("a2dp-sink-sbc_xq"));
    assert_eq!(world.status("gram")["model_number"], "A3064");
}

#[test]
fn a25_sustained_capture_counts_gaps_without_sleeping() {
    let mut world = World::pair();
    world.open_case();
    world.offer_profiles("gram", stereo_catalog());
    world.play("gram");
    world.mic("gram", 1, false);
    world.tone("gram", 120_000);
    world.advance(120_000);
    world.settle();
    assert!(world.mic_samples("gram") > 0, "120s of tone should produce samples");
    assert_eq!(world.selected_profile("gram").as_deref(), Some("a2dp-sink-sbc_xq"));
    assert!(world.discovered("gram", "xps"), "fleet discovery stays up during capture");
}

#[test]
fn a26_mute_stop_and_reconnect_follow_the_control_link() {
    let mut world = World::single();
    world.open_case();
    world.offer_profiles("gram", stereo_catalog());
    world.play("gram");
    world.mic("gram", 1, false);
    world.tone("gram", 200);
    world.settle();
    assert!(world.mic_samples("gram") > 0, "capture should deliver samples");
    world.mic("gram", 1, true);
    world.settle();
    assert_eq!(world.mic_samples("gram"), 0, "mute stops capture");
    world.mic("gram", 0, false);
    world.fail_connect("gram");
    world.settle();
    world.command("gram", "connect");
    world.settle();
    assert_eq!(world.stage("gram"), "ready");
    assert_eq!(world.selected_profile("gram").as_deref(), Some("a2dp-sink-sbc_xq"));
}

#[test]
fn a27_missing_battery_is_not_zero_and_a_stale_lid_is_not_live() {
    let mut world = World::single();
    world.close_case();
    world.settle();
    assert!(world.status("gram").get("left").is_none());
    world.battery("gram", Slot::Left, 0, false);
    world.battery("gram", Slot::Right, 9, true);
    world.battery("gram", Slot::Case, 40, true);
    world.settle();
    assert_eq!(world.status("gram")["left"]["level"], 0);
    assert_eq!(world.status("gram")["left"]["available"], true);
    assert_eq!(world.status("gram")["right"]["level"], 9);
    assert_eq!(world.status("gram")["case"]["level"], 40);
    world.advance(130_000);
    world.settle();
    assert_eq!(world.status("gram")["lid_state"], 2, "a stale lid is not a live closed case");
    assert_ne!(world.status("gram")["last_known_lid_state"], 2);
}

#[test]
fn a28_a_restart_discards_old_callbacks() {
    let mut world = World::single();
    world.open_case();
    world.command("gram", "connect");
    world.suspend("gram");
    world.settle();
    world.restart("gram");
    world.fail_connect("gram");
    world.settle();
    world.command("gram", "connect");
    world.settle();
    assert_eq!(world.stage("gram"), "ready");
    assert_eq!(world.status("gram")["control_connection"]["reason"], "");
    assert!(world.seat_up("gram"));
}
