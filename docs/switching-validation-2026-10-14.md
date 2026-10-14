# Switching validation, 2026-10-14

This check used XPS, LG Gram, and AirPods Pro 3 with the current daemon and plugin changes. It exercises connection state and routing on real hardware separately from the simulated acceptance scenarios.

## Race found and corrected

During the initial repeated Reconnect checks, Gram established its control connection beside an Apple device. XPS lost its background connection, then desktop activity immediately opened it again. The AirPods closed Gram’s new control connection. One preparatory takeover timed out; a later retry succeeded.

Background attachment and desktop activity now share the slot holds. An unexpected loss of a ready connection starts a five-second settling window. The existing 30-second hold after yielding a slot and the owner-first reopen hold also apply to disconnected activity. Repeated close callbacks cannot extend the settling window, and stale generations cannot change it. Lid-triggered background attachment uses the same gate.

Activity on an already-ready connection can still claim idle ownership immediately. Explicit Connect and a new Play event can also open a connection during the background hold. Bluetooth bonds, accessory keys, and fleet enrollment are preserved.

## Automated checks

| Check | Result |
| --- | --- |
| Rust library and binary tests | 285 unit tests passed |
| Acceptance scenarios | All 32 passed |
| Plugin model checks | Passed |
| Recorded plugin replays | Both sessions passed |
| Qt 6 QML checks | 177 passed; none failed or skipped |
| Clippy, daemon and xtask targets | Passed with warnings denied |
| Rust formatting, plugin validation, installer shell syntax | Passed |
| Locked release build | Passed |

The acceptance harness was updated to deliver the current observation inputs, including raw ear-placement packets with their observation time. Scenario expectations were unchanged. Three new daemon tests cover the lost-slot race, yielding versus explicit intent, and stale socket callbacks.

The QML suite ran before the daemon fix; no plugin source changed during the fix. These checks establish the tested state transitions and rendering behavior, rather than an exhaustive hardware reliability guarantee.

## Live checks after the correction

Both laptops ran the same corrected release binary. Neither service restarted automatically during the checks.

| Check | Result |
| --- | --- |
| Apple-seat Reconnect | Three cycles from each laptop passed, including every preparatory connection. XPS took 14.5, 13.1, and 14.1 seconds; Gram took 12.4, 15.2, and 12.3 seconds, including two seconds of stable completion |
| Deliberate activity race | Two takeovers in each direction passed. All 33 activity requests on the displaced laptop during setup were ignored. Each incoming connection remained ready for three seconds, retained the Apple peer, and preserved both laptops’ pairing and fleet membership |
| Race-test Reconnect setup | Four additional Reconnect operations completed before the deliberate race checks |
| Browser playback takeover | Three Play/Pause cycles from each laptop passed. Both laptops agreed on the playing owner; the stream reached an unmuted, uncorked AirPods sink reporting RUNNING |
| Protect a seated playing peer | All six activity requests on the other seated laptop were ignored; the playing owner remained unchanged |
| Pause visibility | All six pauses cleared playback on both laptops |
| Idle shared-seat ownership | Eight alternating activity transfers passed in 1.0–1.6 seconds, including stable confirmation. Both control links stayed ready and neither laptop falsely reported playback |
| Pairing and fleet persistence | Bluetooth bonds, AirPods keys, and enrollment remained intact throughout |

The first playback-protection attempt began with Gram disconnected after the race test. It accepted activity to request a background connection, so the test’s expectation of an ignored request did not apply. A following preparation overlapped that route and received a routing-in-progress response. Both laptops were then confirmed ready before repeating the full six-cycle browser and eight-transfer idle sequences above.

The browser stream volumes were restored after the tests. XPS browser playback was restored and Gram was left paused. No Bluetooth device or keyring item was removed.

These checks establish reported ownership, connection stability, and audio routing. They do not establish audible quality, uninterrupted playback from the retained Apple device, or reliability across all AirPods models. Physical suspend/resume and microphone capture were not repeated in this switching run.
