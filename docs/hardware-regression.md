# Hardware regression

Tested on 2026-10-13 using omarchy-xps, lg-gram, and AirPods Pro 3. Both laptops ran the installed FleetPods user service and Omarchy plugin. The initial checks used the release candidate at `ca06464`; the wake correction below was then built and deployed on both machines. This is a hardware check, separate from the unit, acceptance, and QML suites.

## Results

| Check | Result |
| --- | --- |
| Apple-seat Reconnect | Three cycles initiated from each laptop completed. XPS took 8.0, 9.1, and 15.2 seconds; Gram took 6.1, 7.0, and 8.5 seconds. These times include two seconds of stable completion |
| Pairing through Reconnect | Both laptops retained their saved Bluetooth bonds, AirPods keys, and fleet enrollment throughout all six cycles |
| Idle ownership over shared connections | Eight alternating CLI activity requests transferred ownership. Both laptops remained connected and neither falsely reported playback |
| Desktop activity path | A virtual Shift key on each desktop passed through the compositor and installed plugin's idle monitor and transferred ownership. This checked the desktop path in addition to the CLI path |
| Browser playback takeover | Three Play/Pause cycles from each laptop passed. Both laptops agreed on the playing owner, and the browser stream reached an unmuted, uncorked AirPods sink in the RUNNING state |
| Protect a playing peer | Activity on the other laptop was ignored during all six playback cycles; it did not take ownership |
| Pause visibility | All six pauses cleared the playing state on both laptops |
| Bluetooth audio transport | Additional observations confirmed active BlueZ audio transport with locally reported playback on each laptop |
| Microphone capture and reopen | Two 10-second captures on each laptop delivered approximately 640,000 samples at 64 kHz mono, with nonzero PCM. Closing each recorder returned capture to idle. Bluetooth audio stayed in the A2DP profile |
| Microphone packet integrity | No malformed packets, decoder errors, or dropped blocks in the four short captures. Each start counted about 0.48–0.60 seconds of silence while waiting for decoded audio |
| Extended microphone capture | One 20-second capture on each laptop delivered approximately 1.28 million samples. Silence counters stopped advancing after the first 0.56 seconds on XPS and 0.52 seconds on Gram; no further silence replacement, malformed packets, decoder errors, or dropped blocks was observed |
| Gram suspend/resume | Initial check exposed the wake bug below. After correction, a 14-second suspend resumed with a ready control connection, intact pairing and fleet membership, and the same daemon PID. Desktop activity and a five-second microphone capture passed afterward |
| XPS suspend/resume | A 12-second suspend resumed with a ready control connection, intact pairing and fleet membership, and the same daemon PID. Desktop activity and a five-second microphone capture passed afterward |
| Header after peer wake | Gram initially retained “Connecting” despite both control connections being ready. After the header correction below and a shell reload, both live plugins named the confirmed Omarchy fleet owner |
| Process stability | Neither daemon restarted automatically during the checks above |

## Findings and limits

The initial Gram suspend closed its control socket before reporting suspend to the daemon. The daemon interpreted that callback as lost ownership, clearing the state needed for wake recovery. BlueZ reconnected afterward, but the control session stayed idle and the plugin displayed “Connected to lg-gram” with an empty Omarchy seat.

The adapter now reports suspend before closing the old socket. Its close callback belongs to the retired generation and cannot overwrite the suspend state. After the existing wake grace, a previously ready idle connection can restore control when Bluetooth has already rejoined, without requesting audio ownership or peer admission. A Bluetooth-only connection also gets a waiting-for-setup caption rather than a confirmed seat caption. Two new Rust regression tests cover callback order and idle wake recovery. The corrected build passed 265 unit tests, 32 acceptance scenarios, Clippy, the plugin model/replay checks, and all 158 QML checks.

The first post-fix activity observation overlapped fresh XPS input, which took ownership back. Journal events confirmed both desktop activity paths had issued claims. An isolated repeat passed Gram activity and microphone recovery; this was an observation collision rather than another control-link failure.

The peer-wake check also exposed a header bug: a connected laptop's retained playback intent was projected as a pending connection whenever the peer owned audio. That fallback now applies only while the local link is disconnected. Three model assertions cover a ready Gram displaying the returned XPS owner, both playing and idle. The model and replay checks passed, and a live check confirmed matching fleet subtitles on both laptops after reloading their cached shell components.

One XPS Reconnect recovered from a transport failure during the connecting stage. Its control socket returned `Host is down` before the retry succeeded. Several Bluetooth advertisement registrations also failed during that interval. The logs locate the retry before handshake and notification setup, but do not establish whether advertisement contention caused it. All six Reconnect operations recovered without a terminal failure; this is not a claim of retry-free or consistently timed reconnects.

A raw `paplay` stream started on a laptop that did not own the AirPods did not request ownership. The current implementation lets stream-only playback follow an already-owned connection; takeover requires a media-player Play event. A subsequent raw stream on the owning laptop reached the AirPods. The repeated takeover checks therefore used the existing browsers' real MPRIS Play/Pause events. This limitation should be clear in release support notes.

Reconnect was exercised with both buds in an open case and an iPhone holding the other connection. For the shared-seat, playback, and microphone checks, the user wore both buds and disconnected the Apple devices. These checks did not establish uninterrupted Apple playback during a handoff or microphone quality in a real call. Microphone PCM was measured in memory and discarded; no recording was saved.

The tests paused the browsers on completion and restored the playback stream volumes they temporarily reduced. No Bluetooth bond, keyring item, fleet membership, or saved preference was reset.

## Playback output recovery follow-up

Later on 2026-10-13, the user reported Brave going silent during playback on XPS. Brave had an uncorked, unmuted stream linked to the AirPods sink. The sink reported running, BlueZ reported an active transport, and both laptops agreed XPS owned playback. A brief suspend/resume of that sink restored audible playback, confirmed by the user. These observations locate recovery in the audio output path but do not establish the original cause.

The daemon now observes raw sink states and can reopen an idle, suspended or failed output after three seconds of active, unmuted playback on a locally owned connection. Pauses, muted streams, sleep and handoffs are excluded. Recovery is rate-limited and bounded. `fleetpods audio reset` requests the same operation for failures that continue reporting a running output.

The follow-up build passed 269 unit tests, 32 acceptance scenarios and Clippy. Simulated daemon tests cover the recovery delay, repeated failures, peer ownership, pause, suspend and unchanged Bluetooth pairing. The command completed on XPS with the control link ready, AirPods keys intact and fleet membership preserved. Attempts to inject a sustained hardware failure by suspending the sink returned it to running before the watchdog confirmed a persistent fault. They do not establish hardware validation of automatic recovery or reproduce the original silent-but-running failure. Long uninterrupted playback and capture at the next recurrence remain necessary to investigate its cause.

## Remembered fleet connector

Run with the new daemon on every fleet host. With the pods in a closed case, use host A and then host B, allowing a few seconds for radio exchange. Both status files should show B in `fleet_preference.device`, with the same `activity_ms`. Open the case: only B should open a control connection, and availability alone should not move speaker audio to the pods.

Repeat with B asleep before opening the case. A must stay disconnected beyond discovery expiry and the old reconnect grace. Restart A and verify that B remains preferred. Fresh input or explicit Connect on A should make A preferred. A phone already playing on the pods must retain its existing activity protection. Check that a superseded pending connection restores the original mute and volume of any held speaker streams.

This is a hardware checklist, not a claim of completed hardware validation. The automated two-daemon regression covers radio exchange, persisted preference, case reopen, absent owner, unsolicited attach, explicit Connect, and cancellation of a superseded opening attempt. Radio isolation can leave devices with different last-heard activity; do not infer consensus from a single host's status.


## Fleet pod-state sharing

- With XPS holding the seat and Gram disconnected, lift/dock each bud and insert/
  remove it from an ear. Compare status `left`, `right`, `lid_state`, and
  `fleet_pod_state` on both hosts. Gram should identify XPS as reporter and use its
  bud readings while remaining disconnected. State changes coalesce within one
  second before the next available radio slot; reception is not guaranteed.
- Check `fleet_pod_state.age_ms` stays below ten seconds while the owner publishes.
  Duplicate packets must not refresh the age. Record arrival latency across
  several transitions; do not infer airtime utilization from payload size alone.
- Let the owner stop reporting. Within ten seconds of its last envelope, Gram
  should return to its own BLE observations (`fleet_pod_state: null`). Explicit
  owner roster withdrawal should stop selection sooner. Reconnect/seat policy
  must remain unchanged.
- Switch the seat in both directions while playing audio. The new owner should
  publish; the old owner should stop. Check handoff completion and audio for
  regressions; telemetry intentionally waits during route exchanges.
- Open/close the case where only one laptop receives the newest BLE reading.
  A newer local case/lid observation must survive an older fleet observation.
  Closing the case can end the control link; BLE remains the fallback afterward.


## Audio starts before control setup

- With a genuine playback request and the audio profile absent, confirm the
  authorized control attempt starts first and the A2DP profile request follows
  as soon as Bluetooth is connected, without waiting for control setup. Compare
  first active audio transport with the later control-ready event; startup time
  still depends on the AirPods and Bluetooth controller.
- Repeat after opening the case, an explicit Connect/Play, and a fleet handoff.
  Admission and preferred-connector policy must still gate opening a connection.
  Background desktop activity alone must not request an audio profile.
- While audio is streaming and control reconnects, verify the panel keeps the
  XPS seated and shows Playing, with no connecting spinner. Pausing or dropping
  the audio transport should restore ordinary connection progress if control
  is still opening. Raw status must retain the true control stage.

## Routine advertisements during connection attempts

- With the iPhone connected and the XPS disconnected, press Play while the pods
  are available. Check for `routine fleet advertisements deferred while Bluetooth
  connects`, followed by `resumed` when Bluetooth connects. Routine presence,
  roster refreshes and pod telemetry should stop after the current 500 ms slot;
  incoming discovery must remain active. Audio profile setup should still start
  on the Bluetooth-connected event, before control readiness.
- Repeat with a failed attempt and with explicit Disconnect during an attempt.
  Routine broadcasts must resume in retry backoff or after cancellation, then
  defer again when a retry actually starts. No extra connection attempt should
  be created by advertisement scheduling.
- Repeat a fleet handoff. Routing packets, their authentication presence,
  connector preference updates, enrollment and the complete three-copy roster
  change burst must still air during connection attempts.
- Compare Play-to-Bluetooth and Play-to-active-transport timings over repeated
  case-open cycles. The 2026-11-01 XPS incident included a roughly five-second
  failed first attempt and concurrent advertisement-registration failures.
  Those logs do not prove radio contention caused the failure; improved success
  rate and latency from this scheduling change still require hardware testing.

Deployment smoke check on 2026-11-01: XPS and Gram ran the same release daemon
binary, both services stayed active and answered status requests, and Gram
re-established control while reporting XPS as the playing owner. The build passed
380 unit tests, 33 acceptance scenarios and Clippy. This was not a repeated
case-open timing test.

## Player resume after taking playback back

On 2026-11-01 the user reported YouTube losing sync when XPS took playback back
from Gram. XPS logged a Bluetooth audio transport error at 09:07:32, then a
rebuild at 09:07:38. The profile-reset reply arrived at 09:07:38.730; FleetPods
resumed Brave at 09:07:38.760, while its stream was still on the muted speaker
output. The stream returned to the pods at 09:07:39.734 and the PipeWire Start
command completed at 09:07:40.007. This establishes an early-resume race, not a
measurement of the perceived lip-sync offset.

Recovery now pauses playing MPRIS players and holds their resumes through relink
and the Start reply. Regression coverage includes closed browser streams, failed
starts, stale and duplicate replies, cancellation, audio-server loss, and timeout.
For hardware validation, play on Gram and then press Play on XPS. Confirm the
XPS player pauses during any rebuild, its Play command follows the successful
output Start completion, and the video and sound stay synchronized. Repeat in
both directions and confirm normal Play on an already active output does not
introduce an extra pause. Perceived synchronization still needs listener validation.

The corrected build passed 381 unit tests, 33 acceptance scenarios and Clippy.
It was deployed to XPS and Gram; both services ran the matching binary and
answered status requests. This deployment check did not measure lip sync.

## Case insertion while speaker playback continues

The 2026-11-01 09:39:48 XPS attempt failed with `Host is down` after 5.1 seconds.
The next attempt began at 09:39:54.396; Bluetooth connected at 09:39:58.525 and
audio became active at 09:40:03.440. Speaker playback was muted at 09:39:53.197.
There was no output rebuild or MPRIS pause from FleetPods in this sequence, so
the handoff-resume correction was not involved.

Periodic connector-preference envelopes still bypassed advertisement deferral
every three seconds. They now count as routine traffic; new preference records
remain coordination traffic. Queued envelopes coalesce without dropping an
unsent change's priority. Regression coverage checks that repeated refreshes
produce no advertisements during a simulated connection attempt, genuine
changes still air, and broadcasts resume after connection. The build passed
384 unit tests, 33 acceptance scenarios and Clippy.

Repeat this insertion scenario and inspect the new advertisement-release and
changed-preference logs alongside connection timing. Even this narrower exception
does not establish that advertising caused the failed first attempt. A cold
connection still uses the existing muted-stream hold while output setup proceeds.

## Rejoining after the phone reconnects

The 2026-11-01 XPS capture established two separate delays. On the first
connection, BlueZ connected at 10:02:21.588 and the audio profile completed at
10:02:23.200. PipeWire restarted its device-profile timer at 10:02:23.725;
the timer expired at 10:02:25.725 and the output node appeared at 10:02:25.740.
This identifies a two-second profile wait in this capture, not the suspected
three-second codec reconfiguration rate limit. Audio became active at
10:02:27.120. These observations do not establish the cause of every earlier delay.

Later, the controller reported `Remote Device Terminated due to Low Resources`
(0x14) at 10:02:53.195. FleetPods paused Brave and rejoined after its existing
two-second grace. At 10:02:57.716 it resumed Brave without sending an ownership
claim: Brave had acknowledged the earlier pause, so the confirmation predicate
no longer saw playback intent. An iPhone-owner report 101 ms later caused
FleetPods to pause Brave again. The user pressed Play at 10:03:00.787.

Queued resumes now count as playback intent for ownership confirmation. The
regression test acknowledges the browser pause before setup completes, checks
that the claim is sent, and delivers the repeated remote-owner report before
the grant. The existing one-rejoin limit still applies. Repeat the case-to-ear
sequence with the iPhone reconnecting after XPS and verify that recovery sends
the claim and playback continues without another manual Play. Passing the
simulated sequence does not establish a successful hardware retest.

## Completing hands-free setup before the profile wait expires

Two temporary XPS trials on 2026-11-01 tested a companion HFP request without
changing playback codec or microphone selection. Requesting HFP immediately
after A2DP returned at 10:09:37.923 failed with `br-connection-busy` because
service discovery was still running. That cycle took 9.82 seconds from the
control connection attempt to active audio.

Waiting for both A2DP success and `ServicesResolved=true` worked: discovery
finished at 10:10:59.058, HFP replied at 59.286, both profiles were ready at
59.530, and the output appeared at 59.555. PipeWire stopped its profile timer
without waiting for expiry. Active audio followed at 10:11:01.090, 7.77 seconds
after the connection attempt. The user reported faster playback. Total times
also include variable Bluetooth negotiation; this is one successful trial,
not a guaranteed two-second improvement on every connection.

FleetPods now requests HFP once after a successful local A2DP request and
completed discovery, when the accessory advertises HFP support. The request
belongs to the current connection generation and is discarded on control release or
disconnection. Failure is optional and does not fail A2DP or create a retry loop.
Repeat case insertion and verify the hands-free request follows discovery,
the playback profile remains A2DP, and the profile wait finishes early.

The same investigation caught a separate pause at 10:09:23.417: after the
ear-removal pause, the user resumed Brave on the speakers with both pods
docked. Losing the remaining control link paused Brave again. A link that
has already released audio now closes without handing off speaker playback.
The regression test verifies no extra pause/mute and preserves the deferred
claim for the next insertion. Ear-removal pause behavior is unchanged.

## 2026-11-12: XPS/Gram background connection churn

Both laptops repeatedly lost their ready control link beside the iPhone, waited
five seconds and reopened in the background. Each still remembered itself as
preferred. The existing `displaced` state was recorded but passive reconnects
did not consult it. Repeated Max wear advertisements could therefore authorize
another opening as soon as the settling delay elapsed.

Passive reopening now honors that displacement state; fresh activity and an
explicit Connect still work, and switching accessories clears the old hold.
The regression reproduced an OpenLink at 5,000 ms before the fix. Afterward it
held through repeated wear reports up to 60 seconds and passive ACL attachment,
then accepted fresh activity/Connect. All 486 unit and 33 acceptance tests and
Clippy passed.

Installed the matching daemon on XPS and Gram, with running executable SHA-256
`989ff832ba2662b790cad73d7c372a5d043b644cc52812d0d3a3e00e46cb1e52`.
Gram was temporarily stopped during diagnosis and restored after installation.
Live checks showed XPS retaining playback and Gram staying connected in the
background; the user confirmed the switching stopped. The FleetPods toast and
battery ring also rendered on Gram's older native notification implementation.
No native Omarchy plugins were edited.

A separate clock check found Gram approximately 16 seconds ahead of XPS,
exceeding the fleet's five-second future-clock allowance. Refreshing Gram's
systemd-timesyncd service over SSH required a sudo password and was not applied.
This remains a potential cause of rejected fleet advertisements.
