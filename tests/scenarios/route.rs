use crate::support::World;

#[test]
fn a11_both_laptops_fill_seats_without_cancelling() {
    let mut world = World::pair();
    world.open_case();
    world.settle();
    world.command("gram", "activity");
    world.command("xps", "activity");
    world.settle();
    assert_eq!(world.connect_attempts("gram"), 0);
    assert_eq!(world.connect_attempts("xps"), 0);
    world.advance(1_999);
    world.settle();
    assert_eq!(world.connect_attempts("gram"), 0);
    assert_eq!(world.connect_attempts("xps"), 0);
    world.advance(1);
    world.settle();
    assert!(world.seat_up("gram") && world.seat_up("xps"));
    assert_ne!(world.phase("gram"), "cancelled");
    assert_ne!(world.phase("xps"), "cancelled");
    assert_eq!(world.connect_attempts("gram"), 1);
    assert_eq!(world.connect_attempts("xps"), 1);
}

#[test]
fn a12_a_transient_connect_retries_in_the_open() {
    let mut world = World::single();
    world.open_case();
    world.settle();
    world.command("gram", "activity");
    world.settle();
    assert_eq!(world.connect_attempts("gram"), 0);
    world.advance(2_000);
    world.fail_connect("gram");
    world.settle();
    assert_eq!(world.connect_attempts("gram"), 1);
    assert_eq!(world.stage("gram"), "retrying");
    assert_ne!(world.stage("gram"), "failed");
    let deadline = world.status("gram")["control_connection"]["next_retry_ms"].as_u64();
    assert!(deadline.is_some(), "retry deadline is visible");
    world.advance(999);
    world.settle();
    assert_eq!(world.connect_attempts("gram"), 1);
    world.advance(1);
    world.settle();
    assert_eq!(world.connect_attempts("gram"), 2);
    assert_eq!(world.stage("gram"), "ready");
    assert_eq!(world.status("gram")["control_connection"]["reason"], "");
}

#[test]
fn an_idle_laptop_observes_an_open_case_without_joining_the_active_peer() {
    let mut world = World::joined_pair();
    world.close_case();
    world.settle();
    world.open_case();
    world.settle();
    world.command("xps", "activity");
    world.run_for(60_000);
    assert!(world.seat_up("xps"));
    assert!(!world.seat_up("gram"));
    assert_eq!(world.connect_attempts("gram"), 0);
    // Local input still joins through the connected fleet helper.
    world.command("gram", "activity");
    world.run_for(5_000);
    assert!(world.seat_up("gram"));
}

#[test]
fn a13_admission_keeps_a_helper_when_a_slot_is_free() {
    let mut world = World::joined_pair();
    world.open_case_on("gram");
    world.command("gram", "connect");
    world.run_for(12_000);
    assert!(world.seat_up("gram"));
    assert!(!world.seat_up("xps"));
    world.command("xps", "activity");
    world.run_for(5_000);
    assert!(world.seat_up("gram"), "a free slot must not drop the helper");
    assert!(world.seat_up("xps"));
    assert_eq!(world.status("xps")["fleet_outcome"]["result"], "joined_alongside");
}

#[test]
fn a14_busy_or_dropped_ack_stays_visible_and_does_not_repeat() {
    let mut world = World::joined_pair();
    world.open_case_on("gram");
    world.command("gram", "connect");
    world.run_for(12_000);
    assert!(world.seat_up("gram"));
    assert!(!world.seat_up("xps"));
    world.drop_route_frames("gram", 2);
    world.command("xps", "activity");
    world.command("xps", "activity");
    world.settle();
    assert_eq!(world.status("xps")["fleet"]["routing"]["phase"], "waiting_admission");
    assert_eq!(world.connect_attempts("xps"), 0, "wait for the peer before opening");
    world.run_for(5_000);
    assert!(world.dropped_fragments() >= 2, "the admission replies were actually dropped");
    assert!(world.seat_up("xps"));
    assert_eq!(world.connect_attempts("xps"), 1, "a duplicate request must not open another link");
}

#[test]
fn a15_mouse_activity_does_not_take_the_iphone() {
    let mut world = World::single();
    world.remote_playing("iphone");
    world.settle();
    world.command("gram", "activity");
    world.settle();
    assert_eq!(world.holder("gram"), "remote");
    assert_eq!(world.status("gram")["remote_playing"], true);
    assert_eq!(world.status("gram")["ownership_intent"], "none");
}

#[test]
fn a16_local_play_takes_audio_on_both_panels() {
    let mut world = World::pair();
    world.open_case();
    world.remote_playing("iphone");
    world.settle();
    world.advance(2_000);
    world.settle();
    world.play("xps");
    world.settle();
    assert_eq!(world.holder("xps"), "local");
    assert_eq!(world.holder("gram"), "remote");
    assert!(world.seat_up("xps"));
    assert_eq!(world.selected_profile("xps").as_deref(), Some("a2dp-sink-sbc_xq"));
}

#[test]
fn a17_repeated_switches_do_not_loop() {
    let mut world = World::pair();
    world.open_case();
    world.settle();
    world.advance(2_000);
    world.settle();
    for _ in 0..3 {
        world.play("gram");
        world.settle();
        world.play("xps");
        world.settle();
        world.remote_playing("iphone");
        world.settle();
    }
    world.play("gram");
    world.settle();
    assert_eq!(world.holder("gram"), "local");
    assert_eq!(world.holder("xps"), "remote");
    assert!(world.connect_attempts("gram") <= 4);
    assert!(world.connect_attempts("gram") >= 1);
}

#[test]
fn a18_profile_in_progress_is_not_a_control_failure() {
    let mut world = World::single();
    world.open_case();
    world.command("gram", "connect");
    world.activate_audio("gram");
    world.settle();
    assert_ne!(world.stage("gram"), "failed");
    assert_eq!(world.stage("gram"), "ready");
    assert_eq!(world.status("gram")["control_connection"]["reason"], "");
}

#[test]
fn a19_apple_reconnect_drop_is_not_an_unexpected_failure() {
    let mut world = World::single();
    world.open_case();
    world.command("gram", "connect");
    world.settle();
    let reply = world.command("gram", "reconnect-peer");
    assert_eq!(reply.trim(), "accepted");
    world.fail_connect("gram");
    world.settle();
    assert_ne!(world.stage("gram"), "failed");
    assert_eq!(world.status("gram")["control_connection"]["peer_reconnect"], true);
    assert_eq!(world.status("gram")["control_connection"]["reason"], "");
}

#[test]
fn a20_stale_and_malformed_fleet_traffic_does_not_stick() {
    let mut world = World::joined_pair();
    world.open_case_on("xps");
    world.command("xps", "connect");
    world.run_for(12_000);
    world.play("xps");
    world.run_for(12_000);
    let captured = world.advertisements("xps");
    assert!(!captured.is_empty());
    world.advance(60_000); // Beyond presence, roster and route freshness windows.
    world.restart("gram");
    world.settle();
    for frame in captured {
        world.inject(&frame);
    }
    world.inject(&[5, 0x00, 0x01]); // Truncated authenticated routing frame.
    world.inject(&[0xff, 0x00, 0x01]);
    world.settle();
    assert_ne!(world.holder("gram"), "remote", "stale traffic cannot restore ownership");
    assert!(!world.seat_up("gram"));
    world.run_for(12_000); // Fresh authenticated reports can restore the remote view.
    assert_eq!(world.holder("gram"), "remote");
    assert!(!world.seat_up("gram"), "a replay must not invent a seat");
    assert_eq!(world.connect_attempts("gram"), 0);
    let line = world.command("gram", "status");
    assert!(!line.contains("GroupKey"));
    assert!(!line.to_lowercase().contains("irk"));
}
