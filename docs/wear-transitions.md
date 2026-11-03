# Wear and case transition audit

Reviewed 2026-11-03 after manual Play with an off-head Max kept targeting its
still-connected Bluetooth output. This audit covers the daemon's placement
consumers, Bluetooth observations, peer reports and panel projections.

## Shared playback decisions

`Accessory::playback_blocked` owns positive evidence that a new play belongs on
speakers: a confirmed stowed case, both buds docked, or a headset confirmed off-head.
Unknown wear is not removal. This is separate from `playback_available`, which
requires recent positive evidence before acquiring a disconnected accessory.

| Transition or decision | Owner | Headset equivalent |
| --- | --- | --- |
| Decode placement | `pods::setup`, `pods::air`, `step::apply_observation` | AACP headset wear preserves both cup readings; verified Max 2 BLE wear remains an aggregate fallback. No invented bud or case readings. |
| Choose authoritative wear | `Accessory` | Control wear outranks BLE until link loss or accessory switch; BLE expires independently. |
| Removal pauses playback | `Audio::ears` / `settle` | Same player ownership and resume list; headset removal settles after 100 ms, buds after 400 ms. |
| Re-wear resumes our pause | `Audio::settle` | Shared resume policy; no resume of a player we did not pause. |
| Insertion while speakers play | `Daemon::note_wearing`, `follow_insertion` | Shared aggregate wear transition and takeover gates; removal clears pending insertion. |
| Manual Play while removed | `claim_playback`, `route_owned_playback` | Fixed: both player events and new/uncorked audio streams return to speakers even with a ready Bluetooth link. |
| Deferred Play / snapshot restoration | Session | Shared playback block prevents reclaim while removed; worn evidence enables the existing acquisition path. |
| Idle desktop activity claims ownership | `claim_idle_on_activity` | Fixed: off-head wear blocks an idle claim just as docking does. |
| Publish takeover intent | `roster_intent` | Fixed: a deferred speaker play cannot ask another host to yield while the selected accessory is removed. |
| Publish playing state | `introduce_peers`, `Seat::resolve` | Fixed: confirmed removal suppresses stale playing observations without discarding the connected owner. |
| Automatic/manual output repair | `can_repair_output` | Fixed: off-head wear blocks new repair attempts, as docking does. |
| Rejoin after losing a playing connection | `advance_rejoin` | Fixed: confirmed off-head wear cancels the pending rejoin before opening, just as docking does. |
| Background reopen after Apple reconnect failure | Session | Uses the same availability and playback-block decisions. |
| Switch selected accessory when worn | `track_other`, `maybe_promote` | Shared `wearing()` observation; the current worn/playing accessory remains protected. |
| Control loss after speaker fallback | `control_socket_changed` | Shared released-audio guard; losing the unused control link must not pause speaker playback. |

The speaker regression exercises removal, automatic pause, manual Play, stable
speaker routing on ticks, and re-wear. Both earbud and headset cases run through
the same scenario. A second scenario covers an inherited AirPods default output,
with and without MPRIS. Repair coverage uses a fully streaming connection before
removal and checks both watchdog and explicit repair behavior.

## Deliberate differences

- A closed earbud case gates link attempts and preserves stowed knowledge while
  advertisements disappear. Off-head does not mean the Max is powered down or
  in its Smart Case; it does not tear down a ready control connection.
- Opening an earbud case permits background discovery and applies the case-open
  delay. Headsets instead use wear/availability; they have no lid transition.
- Case battery, charging-based docking, first-pairing case focus, case artwork and
  lid guidance remain earbud-specific.
- The shared one/two-part removal policy uses individual buds or cups from control
  reports. Max BLE only confirms aggregate wear; it cannot distinguish a cup lift.
  One cup still worn is enough to keep manual Play on the Max; only confirmed
  full removal enables speaker fallback.
- The verified BLE layout and independent cup policy are Max 2. Older and
  unidentified headsets retain the primary control reading as aggregate wear;
  an unverified second byte cannot disable their pause policy. Raw cup readings
  remain available if model identity arrives later. Older Max BLE availability
  remains unchanged. Compatibility tests are synthetic, not older-model captures.
- Explicit connection/priority commands and pending recovery/rejoin lifetimes
  are not converted into physical disconnections on removal. These paths are
  shared with buds; removal itself still pauses rather than changing outputs.

## Remaining parity gaps

1. **Fleet telemetry:** `fleet::pod_state::State` carries bud placement and
   case telemetry, but no headset battery/wear. A host without its own Max
   observation cannot display the owner's head-wear reading. This channel is a
   panel projection only; it must remain unable to acquire a seat or drive
   pause/resume. Add explicit timestamped headset fields with a compatible
   protocol evolution, rather than encoding wear as fake bud placement.
The panel now preserves `headset.worn` as true/false/unknown independently of
battery and uses it for the existing unavailable-output guidance. The QML panel
reads `pods.view.headset.worn` independently of `pods.view.connected`; a future
mesh can use true for upright, false for flat, and null for unknown without
changing the connection lifecycle. One cup still worn counts as true.
Off-head does not imply disconnected or in a case. Fleet telemetry remains follow-up work;
this audit does not claim complete UI parity.

## Max 2 cup capture, 2026-11-03

XPS captured AACP EARS packets with prefix `04 00 04 00 06 00`:

| Action | Payload |
| --- | --- |
| Both cups worn | `00 00` |
| Lift left cup | `01 00` |
| Lift right cup | `00 01` |
| Fully removed | `01 01` |

The old parser limited wear to the single headset battery entry and dropped the
second reading. Setup now emits both cup observations independently of battery
updates, including when the wear packet precedes battery identification.
Regression coverage exercises both lifts, full removal, stale BLE interference,
and replacing only one cup before replacing both. The user confirmed a cup lift
pauses playback; manual Play with one cup lifted stays on the Max, while manual
Play with both cups off routes to the speakers. That distinction is intentional
and covered for both left- and right-cup lifts.
