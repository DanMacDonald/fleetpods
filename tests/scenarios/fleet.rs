use crate::support::World;

fn user_enroll(world: &mut World) {
    world.invite("gram", "xps");
    world.run_for(12_000);
    if let Some(code) = world.displayed_code("gram") {
        world.submit("xps", "gram", &code);
        world.run_for(12_000);
    }
}

fn both_hold_each_other(world: &World) -> bool {
    world.enrolled("gram")
        && world.enrolled("xps")
        && world.lists_member("gram", "xps")
        && world.lists_member("xps", "gram")
        && !world.seat_up("gram")
        && !world.seat_up("xps")
}

#[test]
fn a01_closed_case_shows_hosts_and_invites_without_a_seat() {
    let mut world = World::pair();
    world.close_case();
    world.run_for(12_000);
    assert!(!world.seat_up("gram") && !world.seat_up("xps"));
    assert!(world.discovered("gram", "xps"), "xps should be a nearby host");
    assert!(world.discovered("xps", "gram"), "gram should be a nearby host");
    world.invite("gram", "xps");
    world.run_for(12_000);
    assert_eq!(world.phase("xps"), "incoming");
    assert!(matches!(world.phase("gram").as_str(), "inviting" | "connecting"));
    assert!(!world.seat_up("gram"));
}

#[test]
fn a02_enrollment_without_pods_lists_both_members() {
    let mut world = World::pair();
    world.close_case();
    user_enroll(&mut world);
    assert!(both_hold_each_other(&world));
}

#[test]
fn a03_cancel_then_invite_again_can_finish() {
    let mut world = World::pair();
    world.close_case();
    world.invite("gram", "xps");
    world.run_for(12_000);
    world.cancel("xps");
    world.run_for(12_000);
    user_enroll(&mut world);
    assert_eq!(world.phase("gram"), "success");
    assert!(both_hold_each_other(&world));
}

#[test]
fn a04_reverse_invite_after_cancel_has_no_abandoned_key() {
    let mut world = World::pair();
    world.close_case();
    world.invite("gram", "xps");
    world.run_for(12_000);
    world.cancel("gram");
    world.advance(1_000);
    world.run_for(12_000);
    world.invite("xps", "gram");
    world.run_for(12_000);
    if let Some(code) = world.displayed_code("xps") {
        world.submit("gram", "xps", &code);
        world.run_for(12_000);
    }
    assert!(both_hold_each_other(&world));
    assert_eq!(world.status("gram")["fleet"]["error"], "");
}

#[test]
fn a05_restart_does_not_keep_an_empty_fleet() {
    let mut world = World::pair();
    world.close_case();
    world.invite("gram", "xps");
    world.run_for(12_000);
    world.cancel("gram");
    world.restart("gram");
    world.restart("xps");
    world.run_for(12_000);
    world.invite("xps", "gram");
    world.run_for(12_000);
    if let Some(code) = world.displayed_code("xps") {
        world.submit("gram", "xps", &code);
        world.run_for(12_000);
    }
    assert!(both_hold_each_other(&world));
}

#[test]
fn a06_wrong_code_then_right_code_on_the_same_invitation() {
    let mut world = World::pair();
    world.close_case();
    world.invite("gram", "xps");
    world.run_for(12_000);
    let code = world.displayed_code("gram").expect("invitation code");
    let wrong = if code == "999999" { "999998" } else { "999999" };
    world.submit("xps", "gram", wrong);
    world.run_for(12_000);
    let attempt = world.status("xps")["fleet"]["code_attempt"].as_u64().unwrap_or(0);
    assert!(attempt >= 1, "code_attempt should move");
    assert_ne!(world.phase("xps"), "success");
    world.submit("xps", "gram", &code);
    world.run_for(12_000);
    assert!(both_hold_each_other(&world));
}

#[test]
fn a07_five_wrong_codes_and_a_replay_do_not_enroll() {
    let mut world = World::pair();
    world.close_case();
    world.invite("gram", "xps");
    world.run_for(12_000);
    let code = world.displayed_code("gram").expect("invitation code");
    let wrong = if code == "000000" { "000001" } else { "000000" };
    for _ in 0..5 {
        world.submit("xps", "gram", wrong);
        world.run_for(12_000);
    }
    assert_eq!(world.phase("gram"), "error");
    assert!(!world.enrolled("gram") && !world.enrolled("xps"));
    world.replay_letter("gram", "offer");
    world.run_for(12_000);
    assert!(!world.enrolled("gram") && !world.enrolled("xps"));
}

#[test]
fn a08_opposing_invites_and_a_late_cancel_stay_isolated() {
    let mut world = World::pair();
    world.close_case();
    world.invite("gram", "xps");
    world.invite("xps", "gram");
    world.run_for(12_000);
    let stale = world.invitation_id("gram").unwrap_or_default();
    world.command("xps", &format!(r#"fleet:{{"action":"cancel","id":"{stale}"}}"#));
    world.run_for(12_000);
    assert_ne!(world.phase("gram"), "success");
    assert_ne!(world.phase("xps"), "success");
    user_enroll(&mut world);
    assert!(both_hold_each_other(&world));
}

#[test]
fn a09_a_lost_final_ack_survives_restart() {
    let mut world = World::pair();
    world.close_case();
    world.drop_letter("xps", "done");
    user_enroll(&mut world);
    assert!(world.dropped_fragments() > 0, "the final acknowledgement really was lost");
    assert!(both_hold_each_other(&world));
    assert_ne!(world.phase("gram"), "success", "no success without final acknowledgement");
    let saved = world.saved_fleet("gram").expect("verified membership was saved");
    world.restart("gram");
    world.restart("xps");
    world.run_for(12_000);
    assert!(both_hold_each_other(&world));
    user_enroll(&mut world);
    assert!(both_hold_each_other(&world));
    for host in ["gram", "xps"] {
        let after = world.saved_fleet(host).expect("membership survives retry");
        assert!(
            after.id == saved.id && after.key == saved.key,
            "retry must preserve the fleet identity and key"
        );
    }
}

#[test]
fn a10_populated_inaccessible_and_empty_keyrings_differ() {
    let mut populated = World::pair();
    populated.close_case();
    user_enroll(&mut populated);
    populated.lock_keyring("gram");
    populated.restart("gram");
    populated.run_for(12_000);
    let refused = populated.invite("gram", "xps");
    assert!(refused.starts_with("error:"), "an unreadable keyring is not an empty fleet");
    assert!(populated.lists_member("gram", "xps"));

    let mut empty = World::pair();
    empty.close_case();
    empty.restart("gram");
    empty.run_for(12_000);
    user_enroll(&mut empty);
    assert!(both_hold_each_other(&empty));
}
