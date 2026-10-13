# FleetPods daemon and plugin requirements

Status: standalone build specification. This document defines the required finished behavior; implementation progress and test results belong in a separate coverage report.

## 1. Purpose

FleetPods makes Apple's AirPods family switch smoothly between the user's devices. Its Linux daemon owns the Bluetooth and audio integration. The desktop plugin displays its state and sends commands; it does not operate Bluetooth itself.

FleetPods consists of a headless Rust daemon, a command-line client, and an Omarchy desktop plugin. The daemon integrates accessory control, fleet coordination, playback observation, audio routing, and microphone capture.

The first delivery is Linux only. Future macOS and Android fleet participants are a product direction, not a requirement to build their daemons, a platform abstraction framework, or phone applications now. Fleet participants must interoperate through a versioned protocol independently of their host platform or implementation language.

Build the daemon, command-line client, and plugin from the behavior and interface contracts below. No particular source tree, class layout, or previous implementation is required. Document protocol encodings and test them with synthetic fixtures. Validate platform integration and hardware behavior separately from decision logic.

## 2. Scope and assumptions

| Area | Required now |
| --- | --- |
| Host | One Linux user and desktop session, a systemd user service, BlueZ, and PipeWire with WirePlumber |
| Accessory | One configured, already Bluetooth-paired AirPods accessory at a time |
| Fleet | Discover, enroll, remember, and coordinate compatible hosts over Bluetooth LE |
| Desktop | Omarchy plugin communicating through a status file and command interface |
| Playback intent | Browser playback events, MPRIS players, and audio-server observations |
| Audio | Best available supported stereo playback profile; seamless ownership and route changes |
| Microphone | Verified high-resolution AirPods microphone formats while stereo playback continues |
| Settings | Listening and ear-detection controls exposed by the plugin |
| Persistence | Accessory configuration, learned identity, user settings, and authenticated fleet membership |

The two reference hosts are an XPS and an LG Gram. Hostnames, controller indices, audio-node IDs, and device addresses must not be hardcoded. Resolve the configured adapter and accessory using stable identity, not an assumption such as `hci0`.

The reference accessory is AirPods Pro 3. SKU and capability mappings must identify supported AirPods families, but implementation support and hardware verification are different claims. The initial high-resolution microphone target is the format described in section 8; validate it on hardware before claiming support.

Bluetooth bonding, generic Bluetooth discovery UI, output selection, volume sliders, and device forgetting remain responsibilities of the desktop Bluetooth and audio panels. The FleetPods plugin's seat actions are still required; excluding a general Bluetooth manager does not exclude those actions.

Outside the product scope:

- A standalone graphical application, separate system tray, or QR-code UI.
- An Android phone bridge or microphone/audio forwarding to a phone.
- An external Rapport helper. Native AirPods control and Apple-device handoff remain required.
- Multiple local users, multiple simultaneous managed accessories, cloud accounts, a server, or a LAN fleet transport.

## 3. Facts the product must keep separate

| Term | Meaning |
| --- | --- |
| Bluetooth connected | BlueZ reports a link to the accessory |
| Control connected | The AirPods control socket is connected |
| Control ready | The required setup exchange and initial reports have completed |
| Seat | One of the accessory's available host connections, as shown in the plugin |
| Audio owner | The host the accessory identifies as owning audio |
| Audio routed | The local audio server has the intended output and streams routed to AirPods |
| Playing | Evidence of actual playback, not simply a connected socket or selected output |
| Nearby device | A discovered host, possibly unenrolled and not connected to the accessory |
| Fleet member | An authenticated, saved member, even if absent or never seen in an AirPods roster |

A connection request, an accepted fleet message, and a completed connection are separate events. Status must not collapse them into one optimistic `connected` or `playing` flag.

Unknown, stale, absent, and false are different states. A missing report must not become a battery level of zero, a closed lid, a free connection slot, or a claim that a particular device is playing.

### 3.1 Typed state and one owner per fact

Each independently meaningful fact has one canonical representation and one module authorized to change it. Other modules submit observations or requests through that module's interface. They do not maintain their own writable copies. The published JSON in section 10 is a projection of this state, not the internal data model.

Keep these categories distinct in the type system:

| Category | Meaning | Example |
| --- | --- | --- |
| Observation | What a particular source reported, with identity, freshness and connection/session context | BlueZ link established; accessory reported remote ownership |
| Intent | What the user or policy has requested, which may not yet have happened | Prepare a seat; claim audio; release audio |
| Operation | Work in progress, including its identity, phase, deadline and eventual outcome | Connecting, waiting for admission, or retry scheduled |
| Resolved view | A deterministic interpretation of current observations and policy | Effective audio owner and confirmed occupied seats |
| Historical outcome | What a completed operation did at a particular time | Connected alongside another host; previous attempt failed |

Use domain-specific enums and validated values for these categories. For example, a connection operation's retry-scheduled variant carries its deadline, attempt and reason; it is not independently writable `failed`, `retrying`, `completed` and `busy` booleans. Enrollment phases and connection phases are different types even if both contain a waiting state. A terminal outcome cannot simultaneously be a scheduled retry for that same operation.

Unknown observations must be explicit, rather than encoded internally as false, an empty address, or `-1`. An unavailable observation and a retained stale observation may both be useful, but must remain distinguishable. Convert to JSON sentinels only in the serialization adapter. Distinguish host identity from accessory identity, and operation identity from session identity, wherever confusing them could accept a stale result or route a command to the wrong device.

Independent facts remain independent. BlueZ connectivity and control-socket connectivity are different observations, not duplicate representations of one status. Likewise, an accessory ownership report and a local audio-server stream report may legitimately disagree. Retain both with their provenance; one resolution policy interprets them without rewriting the original evidence to force agreement.

### 3.2 State ownership responsibilities

The following assignments are required ownership responsibilities, not a required number of Rust modules, structs, tasks or threads. Responsibilities may share a module. No responsibility may have competing writers or independent recovery loops for the same operation.

| State | Sole ownership responsibility | Other code may |
| --- | --- | --- |
| Local Bluetooth/control lifecycle | Connection module owns observed local link state, setup phase, connection generation and local connection retry schedule | Request connection/release and consume reports; not launch a second connect/recovery loop |
| Accessory reports | Accessory module owns decoded telemetry, settings reports, direct peer roster and reported ownership, including source freshness | Submit received packets or setting requests; not overwrite reported settings with unconfirmed desired values |
| Local desktop audio | Audio module owns observed profile/output/streams, local player observations and stream holds/restoration | Request playback preparation or routing; not maintain a second audio-server connection and competing stream state |
| Nearby hosts | Discovery module owns observed presence and expiry | Consume nearby-host observations; not turn public presence into membership |
| Authenticated peer reports | Fleet observation module owns received peer rosters and audio reports, bound to reporter, accessory, session and freshness | Supply validated messages or consume evidence; not overwrite direct accessory reports |
| User configuration | Configuration module owns the selected accessory and saved preferences | Request validated changes; not treat a saved desired setting as accessory-confirmed state |
| Durable identity and membership | Membership module owns validated fleet credentials/member records and their persistence | Enrollment proposes authenticated changes; discovery and transport read committed membership |
| Enrollment operation | Enrollment module owns invitation identity, role, attempt, authentication phase, expiry and enrollment outcome | Submit invite/code/cancel actions and display progress |
| Handoff/admission operation | Routing module owns handoff intent, fleet transaction phase, admission deadline, transaction retry and completion outcome | Supply activity/play intent and connection results; not independently retry the local connection |
| Microphone capture | Microphone module owns capture demand/lifecycle, decoder, queue and capture recovery budget | Supply control packets and consume PCM; not alter playback-profile policy |
| Effective seats and audio owner | One resolution policy derives these from current direct reports, authenticated fleet reports and local audio observations | Read the resolved view; not independently assign owner or occupied-seat flags |
| Published status | One serialization adapter projects a coherent state snapshot into the published JSON schema | Read it; not feed its derived booleans back into daemon decisions |

Transport reassembly, sequence tracking and retransmission belong to the transport implementation. They deliver validated messages to enrollment/routing; they do not own a second enrollment or handoff outcome. Transaction retries and local connection retries address different operations: a transaction retry must not create another local attempt while the connection module already has that attempt pending.

An operation owner alone changes its phase, arms or cancels its deadlines, and accepts or rejects completion events for its current identity. A timer firing is an event to that owner, not another authority over the state. Keyring and operating-system adapters report actual completion or failure; requesting a write or command does not establish success.

Publish each status document from one coherent logical snapshot. A reader must not receive an ownership field from before a transition and its derived playing/seat fields from after it. Cached derived views are permitted only when invalidated with their inputs and never treated as independent authority. No particular event-bus, actor framework, or central state container is required.

## 4. Startup, presence, and connection recovery

**CON-1. Attach and observe.** Start the daemon without a window. Load the selected accessory and saved user settings, discover available controllers, and observe BlueZ, MPRIS, and the audio server. Attach the control channel to an existing accessory connection when appropriate, including one established through the stock Bluetooth panel. Allow an explicit accessory address at `$XDG_CONFIG_HOME/fleetpods/accessory` (default `~/.config/fleetpods/accessory`). Without an explicit selection, use a single paired AirPods candidate or a single connected candidate among several; do not choose arbitrarily when the result is ambiguous.

**CON-2. Closed case is a normal state.** With the accessory unavailable or its case closed, keep fleet discovery and enrollment available. Publish remembered device identity, known fleet members, nearby hosts, and valid telemetry. Suppress connection attempts when the buds are stowed and unavailable. Closing the lid does not itself sever a live connection: keep occupied seats until disconnection is observed. Show open-lid guidance only once the accessory is disconnected and unavailable.

**CON-3. Finish setup before claiming readiness.** Perform the AirPods handshake, feature and notification setup, and obtain the necessary initial reports. Accept valid reports that arrive earlier than their usual order. Bound every wait. Discard callbacks and timeouts belonging to a superseded connection.

**CON-4. Recover without a new click.** A recoverable initial connection, control-setup, profile, or fleet-admission failure must schedule a bounded retry when the original intent is still valid. Requiring another keyboard event to escape a failed first attempt is a regression. Retry budgets and backoff must avoid continuous radio churn.

**CON-5. Tell the user what is happening.** Publish the attempted operation, current stage, retry count/limit, next retry deadline, and reason for stopping. A transient failure followed by a scheduled retry is progress, not a terminal red error. A retry cancelled before the UI saw the attempt must still have enough context to explain what was cancelled and why.

**CON-6. Avoid false failures.** BlueZ `InProgress` while an audio-profile operation is pending is not evidence that the control connection failed. Serialize or await the relevant operation, then determine its outcome. Publish subsequent success so an earlier audio error cannot remain the apparent current result.

**CON-7. Recover from service changes.** Handle accessory loss, adapter power changes, BlueZ restart, audio-server restart, and suspend/resume. Clean up abandoned operations and rebuild valid state. Do not retain a stale seat or audio owner solely because it was true before the interruption.

**CON-8. Learn accessory secrets through the supported exchange.** Receive and validate the accessory identity-resolution and advertisement-encryption keys through the accessory control protocol, then store them in the keyring. Use them to recognize rotating accessory addresses and decode supported encrypted telemetry while disconnected. Both keys are 16 bytes and both are needed for the pairing-key readiness check. Distinguish keys available in memory from successful durable storage and expose write failures. Before identity keys are available, model-only proximity evidence must not establish confirmed accessory identity or ownership. Missing keys must remain visible as missing; do not mistake another nearby person's AirPods for the configured accessory or require manual secret extraction as the normal setup flow.

Timing must accommodate accessory service discovery and notification setup. Use bounded stage-specific deadlines and backoff, and validate them on the target hardware. Displacement by another host must not start unsolicited reconnect loops: require fresh desktop activity, Play, or an explicit connect action before beginning a new background attempt.

## 5. Fleet discovery and enrollment

**FLT-1. Discover without the pods.** All compatible hosts, including enrolled members, advertise public discovery with their name and Bluetooth address. Keep authenticated presence and routing separate from public discovery. Show discovered, unenrolled Omarchy hosts in the tray with an invitation action, even when the AirPods report them as connected. Do not also draw them in either seat. Unidentified connections have no device icon in the diagram. A public sighting alone never establishes membership or a connection. Use stable host identity despite rotating LE addresses.

**FLT-2. Invite with six digits.** Support both creating a fleet and adding an unenrolled host to an established fleet, subject to the membership limit and authenticated consent. An invitation identifies the intended peer, displays a uniformly generated six-digit code, and expires after two minutes. Preserve leading zeroes. The recipient receives the invitation and expiry through status; the UI owns the six boxes, focus, and automatic submission on the sixth digit.

**FLT-3. Support correction.** A wrong code reports an authentication error and permits another submission within the same live invitation. Increment `code_attempt` so the UI can clear/rearm its input. Use a fresh authentication exchange for each attempt. Allow five attempts; exhaustion is terminal. Do not consume multiple attempts for a duplicate transport delivery or a UI redraw.

**FLT-4. Report real progress.** Distinguish invitation delivery, waiting for code, authentication exchange, saving membership, awaiting peer confirmation, success, cancellation, expiry, and failure. Do not briefly show Finishing fleet setup before code entry. Expose the current protocol phase so the plugin can describe the handshake rather than appearing frozen.

**FLT-5. Cancellation is recoverable.** Either side can cancel. Expiry and cancellation stop the old exchange and suppress stale retransmissions of that invitation. A fresh invitation must appear immediately, including one in the opposite direction. Neither peer disappears from public discovery because an earlier invitation was rejected or expired.

**FLT-6. Do not establish a fleet just by inviting.** A first invitation may hold tentative key material in memory, but must not leave a permanent empty fleet that prevents the host accepting another invitation. Discard unused tentative state on cancellation, expiry, authentication failure, or restart. A saved fleet with no members may be erased; a keyring read failure is not proof that it has no members.

**FLT-7. Save authenticated membership.** Persist an authenticated fleet ID, group key, and member identities at the verified protocol commit points. Both local and peer identities belong in a newly established fleet. Show success only after the authenticated confirmation exchange completes. Saved fleet members must immediately enter the device list, without waiting for the AirPods to share a roster.

Persistence cannot be atomic across two machines. A peer may have saved verified membership when the final acknowledgement is lost. Preserve and reconcile that authenticated membership on a retry or restart; do not erase a real fleet as if it were an abandoned invitation. Matching-fleet retries must be safe and idempotent. A foreign fleet must not overwrite an established fleet silently.

**FLT-8. Bound and protect enrollment.** Limit fleets to 32 members and reject oversized or malformed grants. Merge membership only from authenticated evidence. Opposing or duplicate invitations must not confuse roles, reuse authentication state, replace an established key, or authenticate a peer without code verification.

**FLT-9. Keep secrets out of ordinary state.** Use the user's Secret Service keyring for long-term credentials. If it is unavailable or locked, report a useful error rather than falling back to plaintext or pretending a write succeeded. The invitation code is permitted in owner-readable live status while it is being displayed; fleet keys and accessory pairing secrets are not.

## 6. Fleet seats and seamless switching

**RTE-1. Preparation is not takeover.** Fresh desktop activity may prepare a connection and playback profile. On an existing ready connection it may claim ownership from a peer that explicitly reports idle playback. It must not interrupt a playing or unknown peer, switch the default output, or move playback streams just because the user moved the mouse. Starting the plugin or its activity timer is not fresh user activity and must not claim an idle seat.

**RTE-2. Actual local play intent can take audio.** A genuine media play action can acquire the seat/ownership and route playback to AirPods. Combine browser events, MPRIS, audio-server facts, and accessory reports without treating each observation as a separate takeover. An initial playback snapshot or a buffered stream appearing after a handoff is not automatically fresh play intent.

**RTE-3. Use both available seats.** Two fleet laptops may remain connected to the accessory together. If two slots are available, or neither laptop is connected and occupancy is unknown, their attempts must not cancel or expel each other through fleet messages. Do not add a blanket rule that only one fleet device can connect at a time.

**RTE-4. Ask a connected peer when admission is needed.** A background laptop can send an authenticated request over the fleet transport to a connected laptop. The helper determines whether a free slot already permits connection or whether an accessory routing/priority operation is necessary. Preserve the real playing device. An idle Apple device can yield its seat so the two fleet laptops can connect.

**RTE-5. Coordinate the outcome.** Distinguish request, admission, connection, and completion. Duplicate requests must not repeat a destructive Bluetooth operation. A helper with a free slot must not needlessly disconnect itself. When a write may have succeeded but its acknowledgement/link is lost, verify actual connection state before repeating or undoing it.

**RTE-6. Retry busy or incomplete admission.** A requester that waits for a peer and then fails must either retry with a visible deadline or explain a terminal outcome. Neither machine should silently stop trying while valid connection intent remains. A cancelled, expired, or superseded transaction cannot complete a newer one.

**RTE-7. Share truthful rosters.** Publish authenticated connected-peer and audio-source observations to the fleet. The plugin must identify which laptop is actually playing, including when the local host is not connected. Prefer fresh direct evidence when available; remote reports remain useful before the local initial roster arrives. Expired, pending, mismatched-accessory, or replayed reports cannot establish a confirmed seat or owner.

**RTE-8. Show coexistence.** Expose the peer and whether a connection joined alongside an existing host. This lets the plugin show Connected to AirPods alongside lg-gram, or the corresponding peer event, with its double-ship icon. Do not replace a successful local connection with a misleading Not connected message about the other laptop.

**RTE-9. Respect release.** When another device takes audio, pause all controllable local players and release local routing intent. Hidden browser media without a player interface may continue through speakers; do not indefinitely mute those streams to conceal that limitation. Their continued playback combined with ordinary desktop activity must not reclaim audio from a playing remote owner. A later genuine local play action may claim again. Feedback from the daemon's own pause, stream move, or profile change must not create a takeover loop.

**RTE-10. Reconnect Apple devices.** The plugin can request reconnection of an Apple device through the accessory control protocol. An intentional local disconnect during that sequence is not an unexpected failure. Track success, failure, and recovery of that operation separately from ordinary link loss.

## 7. Playback and listening controls

**AUD-1. Preserve output quality.** Rank available stereo output profiles in this order: SBC-XQ, SBC, then AAC; use profile priority to break ties. Exclude profiles that introduce headset input. Reapply selection on every activation. Absence of a preferred codec must not make another available stereo profile unusable. Distinguish codec ranking from measured negotiated bitrate and audible quality.

**AUD-2. Never solve capture by degrading playback.** Do not automatically select HFP/HSP, mSBC, or CVSD to provide a microphone. The high-resolution control-channel microphone is the supported exception, not a headset profile switch.

**AUD-3. Route deliberately.** Once local playback has legitimately acquired the AirPods, select the intended output and route managed playback streams. Repair an un-routed stream without a needless Bluetooth reconnect. Respect an explicitly selected unrelated output device rather than hijacking every system audio route.

**AUD-4. Do not leak or strand audio.** During acquisition, prevent managed playback from briefly blasting through speakers. Any held/muted streams need bounded release and restoration on success, failure, cancellation, shutdown, and recovery. Preserve original mute and volume, including levels above 100%. A recycled stream identifier must not cause another stream to be restored incorrectly.

**AUD-5. Report successful recovery.** Actual routing/profile success clears the live failure state. The plugin can retain a historical error, but its current status must describe the recovered connection and playback.

**AUD-6. Support the exposed settings.** Persist local listening preferences across daemon restarts. Decode available setting confirmations and distinguish requested, pending, confirmed, and unknown values. Unsupported controls must be disabled or omitted in the plugin. Provide noise modes Off, Noise Cancellation, Transparency, and Adaptive according to model capabilities; adaptive level from 0 to 100; conversation awareness on/off; single-bud noise cancellation on/off; and ear behavior pause on either removal, pause when both are removed, or disabled. Report accessory-confirmed state where the protocol provides it.

**AUD-7. Ear behavior is contextual.** Pause eligible local playback when the configured removal condition is met. Resume only playback the daemon paused, when the configured reinsertion condition is met. Do not resume a player the user independently paused or steal remote audio. Handle closely spaced ear/case reports without transient false transitions.

**AUD-8. Preserve volume behavior.** Apply stem volume changes in five-percentage-point steps. Conversation awareness temporarily reduces playback volume and restores the previous level when it ends. Do not confuse self-generated volume notifications with new user changes or overwrite the saved level with an already-ducked value.

## 8. High-resolution microphone

**MIC-1. Use the normal AirPods input.** The audio panel exposes the normal named AirPods input, not a second High-Resolution Microphone device. A hidden source and WirePlumber integration may implement this. Start capture on recording demand; stop when the last client leaves, input is muted, or the control connection is lost. Honor input volume and mute without changing the playback route.

**MIC-2. Keep playback running.** Receive AACP `0x58` microphone data over the control channel, decode AAC-ELD, and deliver mono PCM while A2DP stereo playback continues. Capture start and stop must not reset the playback profile or interrupt playback.

**MIC-3. Separate the two rates.** Decoder coding rate and PCM delivery rate are distinct. Use the model format table, validate the decoded format, and let the audio server convert to a recorder's requested rate. Neither FFmpeg's coding-rate report nor an audio-port rate establishes the accessory's delivery clock.

| Model | SKUs | AudioSpecificConfig | Coding rate | PCM delivery rate | Samples per frame |
| --- | --- | --- | --- | --- | --- |
| AirPods Pro 3 | A3063, A3064, A3065 | `f8e63000` | 48 kHz | 64 kHz | 480 mono |

Validate the shared format on the supported SKUs. Other models require evidence and an explicit format entry before exposing this microphone. Do not guess a host-specific rate or slow consumption to conceal missing packets, since that changes voice pitch and duration.

**MIC-4. Buffer boundedly.** Use a 200 ms initial/refill reserve and a maximum queue of 65,536 samples to tolerate bursts while bounding latency and memory. After an underrun, re-prime instead of repeatedly emitting tiny fragments. The audio callback must not block on Bluetooth, decoding, allocation, or disk I/O.

**MIC-5. Diagnose loss honestly.** Distinguish incoming packet gaps, decode errors, queue overflow, underflow, and downstream delivery problems. A zero decode-error count is not proof of continuous audio. Include concurrent discovery load in diagnostics; do not attribute packet loss to a kernel-driver defect without evidence.

**MIC-6. Preserve discovery during capture.** Fleet enrollment and presence remain available. Measure capture quality under concurrent Bluetooth discovery. Any scan interval/window tuning is controller-wide and must be documented with its units and duty cycle. Discovery taking a few seconds is acceptable.

Changing those defaults belongs to an explicit installation step with backup and a Bluetooth restart when necessary. The user daemon must not run as root, repeatedly edit system configuration, stop other clients' discovery sessions, or silently restart BlueZ.

**MIC-7. Recover and clean up.** Temporarily disable conversation awareness during capture and restore it afterward. Bound automatic recovery of a stalled microphone; permit two restarts before requiring the recorder to reopen. Remove an unusable source on disconnect and restore availability after control setup. Do not claim uninterrupted recording across source destruction; an existing recorder may need to reopen it.

**MIC-8. Do not record the user.** Microphone PCM is delivered to requesting local audio clients, not saved by the daemon or relayed to a phone. Diagnostic logs contain timing and counters rather than captured speech.

Use WirePlumber 0.5 hooks to block headset switching only for the managed AirPods input, including through daemon restarts. Do not disable headset support for unrelated devices.

## 9. Telemetry and plugin presentation

**TEL-1. Identity and capability.** Publish the accessory's remembered/display name, model, form factor, capabilities, local host identity, and known remote device identities. Do not require a live accessory connection to name an authenticated fleet member.

**TEL-2. Battery.** Publish separate left, right, case, and headset battery availability, level, and charging state as applicable. Distinguish unknown from 0%. The plugin owns the circular indicators and the red fill below 10%.

**TEL-3. Ear, case, and freshness.** Publish ear/case placement and lid observations with validity and timestamps. Separate latest observed lid state from last-known state. Do not infer a live lid state from lack of advertising or unconfirmed empty-case reports. Confirmed docking and connection availability drive the main illustration and seat placement.

**TEL-4. Useful events.** Supply enough structured state for connection attempts, scheduled retries, retry cancellation reasons, terminal failures, recoveries, audio changes, and alongside connections. The plugin owns wording, icons, animated ellipses, countdown rendering, and its recent-activity presentation. Do not add a second UI or a persistent daemon event-history database just to implement those views.

### 9.1 Plugin behavior

Build an Omarchy bar plugin using Quickshell and QML. It reads daemon status and sends commands through the client. Bluetooth, fleet membership, playback policy, and keyring operations remain in the daemon. The plugin owns only presentation state, such as expansion, animation progress, and recent activity history.

**UI-1. Bar and panel.** Show an accessory-state icon in the bar and open the panel on click. The panel contains the accessory name and current status, a device/seat diagram, accessory artwork, battery indicators, controls, fleet enrollment, and a collapsible activity log. Follow the desktop theme and scale without clipping content.

**UI-2. Seats and trays.** Draw Apple and Omarchy groups above the accessory. Devices not confirmed connected belong in their group's tray. A group can display more than one connected device; two fleet laptops may share the Omarchy group. The visual grouping must not imply that a physical slot is permanently reserved for either group. Show playing effects only for the resolved audio owner with current playback evidence. Keep known Apple devices visible in the tray even when the case is closed or the local laptop is disconnected.

**UI-3. Unknown and retained state.** Local disconnection must not invent an Apple-device takeover or present an old Apple owner as newly confirmed. Preserve an unaffected Apple device during a fleet-laptop handoff while valid evidence still supports it; change it when fresh evidence contradicts it or its evidence expires. If occupancy is known but identity is not, use a neutral connected indication without assigning a device. If occupancy is unknown, use an empty neutral seat; avoid redundant Unknown or connect prompts while automatic preparation is available.

**UI-4. Connection actions and guidance.** Distinguish preparing a background connection from claiming audio. Expose explicit connect and Apple-device reconnect actions where applicable. If a manual-connect prompt is needed, use “Play Audio or” on the first line and “click to connect” on the second. Hide or disable it when the closed case makes connection unavailable. In that state, replace the collapsed log area with “Open the lid on the AirPods case to allow Bluetooth connections to the pods.” Keep the panel height equal to its collapsed-log height and wait for actual disconnection before showing that guidance.

**UI-5. Transition continuity.** Animate each confirmed device move once. Repeated or intermediate snapshots must not teleport a device back into a seat just to replay its exit. Devices already in a tray should slide to make room rather than briefly entering the seat. Preserve the diagram and accessory artwork throughout a fast connection. Finish the connector animation smoothly when success arrives; an intermediate disconnected snapshot during the same active operation must not erase the progress animation. A real cancellation or failure must still terminate progress honestly.

**UI-6. Progress and event ordering.** Use “Reconnecting…” throughout an explicit reconnect operation and “Switching…” for a seat transfer. Rotate the arrow icon for active connection/retry progress, including attempts triggered by activity. Do not add a separate Connecting label beneath the battery rings that changes panel height. Start a seat-transition animation with its corresponding log event, rather than displaying completion seconds before the device moves. Fresh fleet updates must update the panel even when the local accessory link is down.

**UI-7. Activity history.** Deduplicate events by operation and transition identity. A coordinated seat transfer should describe the transfer, not emit duplicate generic disconnected messages. Do not log an Apple audio takeover when that device retained audio throughout a laptop handoff. Use the small closed-case bar icon on a case-closed activity line. Retain history within the plugin session; a persistent daemon event database is not required.

**UI-8. Identity and batteries.** Use the same saved device label in the tray, seat, status heading, and history. Learn more specific device names when available and persist them by stable identity; a generic name must not overwrite a more specific one. Never infer a device model solely from its icon or role. Show separate left, right, and case/headset batteries, distinguish unknown from zero, mark retained stale information where applicable, and use the low-battery color below 10%.

**UI-9. Settings and enrollment.** Expose supported listening controls with pending/confirmed feedback. Nearby unenrolled hosts must offer an invitation action even without an accessory link. The invitation dialog shows the host, six-digit code, expiry, and cancellation. The recipient dialog has six boxes, supports paste and leading zeroes, submits once per complete attempt, and clears/rearms after a wrong-code response. Show recoverable errors inline; do not show completion before durable enrollment confirmation.

**UI-10. Process and input behavior.** Opening the panel, initial player snapshots, startup timers, and background polling are not new play intent. Genuine desktop input may request background preparation through the daemon. Handle daemon absence, malformed status, and unsupported schema versions visibly without inventing connected state. Installation must not inject browser code or restart a browser; an optional browser observer requires explicit user setup.

## 10. Local interfaces

The `fleetpodsd` daemon publishes status and accepts local commands. The `fleetpods` client sends commands to the daemon. The plugin and playback observer use these interfaces; they do not directly operate Bluetooth or the keyring.

- User socket: `$XDG_RUNTIME_DIR/fleetpods.sock`. Reject an unavailable/unsafe runtime directory rather than falling back to a shared `/tmp` socket.
- State file: `${XDG_STATE_HOME:-$HOME/.local/state}/fleetpods/status.json`.
- JSON schema version: `1`. Publish atomic replacements on meaningful changes, with owner-only access. Remove stale live status on orderly shutdown.
- One daemon owns this socket, accessory, and audio integration at a time. Do not run competing accessory-management daemons against the same local hardware simultaneously.

### 10.1 Commands and playback inputs

| Command or input | Required effect |
| --- | --- |
| `status` | Return the current JSON snapshot for diagnostics and tests |
| `activity` | Submit fresh desktop activity; report accepted or ignored without claiming connection success |
| `connect` | Request local seat/connection preparation through the same policy used by the panel |
| `reconnect-peer` | Start the Apple-device reconnect operation |
| `handoff:claim` | Explicitly request local audio ownership |
| `handoff:release` | Release local audio ownership |
| `noise:off`, `noise:anc`, `noise:transparency`, `noise:adaptive` | Set a supported noise mode |
| `adaptive:N` | Set adaptive level, accepting only integers in 0–100 |
| `ca:on`, `ca:off` | Set conversation awareness |
| `onebud:on`, `onebud:off` | Set single-bud noise cancellation |
| `ear:one`, `ear:both`, `ear:off` | Set ear-detection behavior |
| CLI `fleet`, with a JSON line on standard input | Dispatch `invite` with address/name, `submit` with invitation ID/code, or `cancel` with invitation ID |
| CLI `fleet reset`, socket `fleet-reset` | Clear this machine's saved and in-memory fleet, preserving AirPods pairing and preferences |
| Socket `playback:` followed by a JSON line | Consume the browser playback observer's events |

The CLI translates fleet input into a `fleet:` socket message. Simple verbs are unframed; JSON message families are newline terminated and bounded to 8 KiB. Handle split reads and malformed input. For the commands above, the CLI waits for replies to `status`, `activity`, `connect`, `reconnect-peer`, `handoff:claim`, `handoff:release`, and `fleet`. Settings commands do not require a reply; their confirmed results appear in status.

Admission/status commands must reply promptly, independently of a long Bluetooth operation. Accepted means work was admitted; hardware completion appears in status. `error:` replies produce a nonzero CLI result and stderr. Timeout or socket closure without an expected reply cannot report success. The CLI waits up to one second for such replies.

Fleet reset is a durable administrative operation and waits for completion, with a CLI timeout of 120 seconds. Serialize deletion with pending enrollment writes so an old key cannot be written back. Delete only the fleet credential, cancel local enrollment and fleet routing work, clear cached fleet rosters and queued advertisements, and leave AirPods keys, Bluetooth pairing, preferences and other machines unchanged. Resetting an already empty fleet succeeds. Report keyring failures explicitly; do not claim success or discard in-memory membership before durable deletion is confirmed.

The playback observer sends `heartbeat`, `reset`, `play`, `pause`, and `snapshot` events with application identity and wall-clock time. Player events also carry a player identifier and deduplication token. Enforce these limits and freshness checks: application up to 256 characters, player/token up to 512, events no more than two seconds old or 250 ms in the future, and a five-second observer heartbeat window. Snapshots update state without manufacturing a new play action. Document response strings and test them with the observer client.

### 10.2 Status fields the plugin consumes

The following groups define the public status schema, not the internal Rust object model. Document wire names, types, optionality, enum values, and unknown sentinels consumed by the plugin, command-line client, and pairing dialog. Keep internal diagnostic fields separate from the public contract.

| Group | Fields / contents |
| --- | --- |
| Identity | `schema_version`, `device_name`, `model_name`, `model_int`, `is_pro_series`, `is_headset`, `local_address`, `local_name` |
| Capabilities | `supports_noise_off`, `supports_noise_control`, `supports_adaptive`, `supports_conversational_awareness`, `supports_one_bud_anc` |
| Link | `connected` for the control socket, `bluez_connected`, `ear_reported`, `pairing_keys_ready`, `control_connection` |
| Settings | `noise_mode`, `adaptive_noise_level`, `one_bud_anc_mode`, `conversational_awareness`, `ear_detection_behavior` |
| Batteries / placement | `left`, `right`, `case`, `headset`; availability, level, charging, and applicable `in_ear` / `in_case` |
| Presence | `lid_state`, `last_known_lid_state`, `lid_updated_ms`, `last_advertisement_ms`, `ble_connection_state`, `case_confirmed_docked`, `waiting_for_playback` |
| Ownership | `reported_ownership`, `audio_source_local`, `audio_source_type`, `audio_owner`, `remote_playing`, `roster_audio_owner`, `remote_source_address` |
| Ownership freshness / intent | `ownership_updated_ms`, `source_updated_ms`, `peer_list_updated_ms`, `ownership_intent`, `handoff_result` |
| Seats | `want_omarchy_seat`, `holding_omarchy_seat`, `remote_peer_count`, `connected_peers` |
| Device list | `devices`: address, label, Apple/Omarchy kind, and discovery marker for unenrolled hosts |
| Local playback | `native_handoff`: enabled, phase, completion, failure, update time, audio state/output/activity, playing and yielded state used by the plugin |
| Enrollment | `fleet`: phase, ID, peer address/name, initiator, expiry, error, code attempt, enrollment state, and invitation code when appropriate |
| Fleet operation | `fleet.available`, `fleet.routing`, `fleet.roster`, and top-level `fleet_outcome` |

`control_connection` provides stage, failed stage, reason, recovery state, retry number/limit, Apple reconnect state/error, and fleet retry/deadline/stop context. Keep timestamps in the units expected by the UI, Unix milliseconds for displayed deadlines and event times. Use monotonic time internally for elapsed-time decisions.

`fleet.routing` exposes phase, busy state, admission state, peer, retained device, and error. `fleet.roster` identifies reporter, accessory, connected peers, audio source, timestamps, pending state, and idle state. `fleet_outcome` preserves timestamp, result, peer, and retained device long enough to survive status polling. Alongside success must remain distinguishable from ordinary success.

Exercise the status schema with fixtures used by the plugin and command-line client. Include incomplete, conflicting, and transitioning states.

### 10.3 Derived summaries are confined to serialization

The JSON schema includes observations, derived summaries, and historical results. Do not create a writable Rust field for every JSON key. Document each emitted field's source or derivation and preserve the distinctions required by its consumer.

| Fields | Required relationship |
| --- | --- |
| `connected`, `bluez_connected`, `control_connection.stage` | Project socket connectivity, BlueZ connectivity and setup/operation phase respectively. These describe different facts and are not interchangeable aliases. |
| `reported_ownership`, `audio_source_local`, `audio_source_type`, `remote_source_address` | Project the corresponding direct accessory evidence and its validity. Do not backfill it from a desired handoff or a guessed effective owner. |
| `audio_owner`, `remote_playing`, resolved seat presentation | Derive through the single resolution policy, preserving the consumer's semantics. A potentially playing remote device need not have a known identity. |
| `roster_audio_owner`, `fleet.roster`, `connected_peers` | Preserve which roster/source supplied the evidence and its freshness. A remote roster and a direct roster are independent observations; reconcile them in one place. |
| `want_omarchy_seat`, `holding_omarchy_seat` | Project requested seat intent and the current seat-hold policy state respectively. Neither alone proves a physical connection. |
| `native_handoff`, `fleet.routing`, retry details in `control_connection` | Project the relevant audio, routing and connection owners. Emit summary flags from their typed phases, not parallel writable flags. |
| `handoff_result`, `fleet_outcome` | Project operation outcomes with their identity/time. A historical failure may remain in history after recovery, but cannot override current successful operation state. |

Where fields differ in meaning, record that difference rather than asserting they must always be equal. Schema tests must check the projection during transitions and contradictory observations, not only a stable connected snapshot. The implementer must provide the field-to-owner/derivation mapping for the complete supported schema as part of the status contract.

## 11. Fleet protocol and security

Fleet participants must enroll, exchange presence, share rosters, and complete routing transactions using the same versioned protocol. Define the service UUID, key derivation contexts, frame encodings, and authentication ordering as stable protocol constants. Incompatible changes require explicit protocol versioning.

| Boundary | Required contract |
| --- | --- |
| Transport | Connectionless Bluetooth LE extended advertisements, service UUID `bf3f771e-2938-4a3e-b227-746d5d706f64`; no LAN, GATT service, or host-to-host Bluetooth bond requirement |
| Enrollment frames | Bounded JSON messages with fragmentation/reassembly, expiry, retransmission and deduplication |
| Code authentication | SPAKE2 with the Ed25519 group and distinct initiator/recipient roles; context binds protocol, invitation, both identities/names and retry attempt |
| Session protection | HKDF-SHA256 key derivation, HMAC-SHA256 confirmation, and ChaCha20-Poly1305 authenticated encryption with directional keys and session-bound associated data |
| Saved membership | 16-byte fleet ID, 32-byte group key, authenticated members; stable Secret Service attributes for credential lookup |
| Presence | Public discovery and encrypted fleet presence; presence carries a routing-session identity |
| Routing | Authenticated request, admission, connection result and completion; versioned packet encoding and result codes |
| Replay protection | Per-session identity and sequence tracking, bounded caches, duplicate-safe responses, freshness checks, restart/key-change invalidation |

Use established cryptographic libraries for the specified authentication and encryption algorithms. Do not implement custom cryptographic primitives or substitute an ad hoc six-digit-code authentication scheme.

Public presence is not authenticated membership. A shared fleet key authenticates fleet participation, not individual member nonrepudiation; do not claim stronger identity guarantees than this trust model provides. Validate source/destination, session, size, timestamps, sequence and authenticated contents before Bluetooth side effects or persistence.

Preserve bounded resource use under malformed advertisements, stale fragments, repeated requests, and cache pressure. A fragment checksum prevents accidental mixing, not forgery. Do not weaken replay protection by silently evicting essential state and accepting old requests as new.

Rotate advertisement fragments at approximately 500 ms and regularly interleave presence. Respect real controller payload limits instead of assuming the advertised maximum is usable. Preserve fair airtime and retry pacing; neither enrollment nor roster traffic should saturate the radio or disable ordinary discovery during microphone capture.

## 12. Runtime, persistence, and diagnostics

- Ship a headless Rust daemon and a small command client compatible with the required commands. Do not require Qt for daemon operation.
- Persist only configuration and state needed by the scoped behaviors. Document configuration paths, keyring lookup attributes, and any supported import or migration operations. Never copy live secrets into the source tree or fixtures.
- Make restart and retry behavior safe after interruption between any two protocol messages or persistence steps. Fail visibly on corrupted/inaccessible saved state; do not silently create a conflicting fleet.
- Bound buffers, pending requests, discovery tables, reassembly and replay state. Release OS resources and discovery ownership on shutdown.
- Provide structured diagnostics for connection stage, transaction, retry reason, profile selection, routing, and microphone packet/queue timing. Redact pairing material and microphone content.
- Keep install-time privileged changes separate from normal operation. Building or testing must not automatically replace the working service or restart Bluetooth.
- Document third-party dependencies and retain applicable licenses, notices, and provenance for any reused code.

Module layout is intentionally unspecified; the typed-state, sole-owner and projection rules in section 3 are required. Choose a small number of deep modules with small interfaces and clear ownership of side effects. Avoid speculative platform layers, duplicate observers/connection owners, and a second implementation of the same audio operation. Test externally meaningful decisions and protocol behavior through module interfaces rather than mirroring private structures.

## 13. Acceptance scenarios

Use deterministic clocks, synthetic identities, packet fixtures, and fake platform boundaries for repeatable tests. A regression test must fail when its relevant protection is removed or defect is reintroduced. Passing unit tests does not establish hardware compatibility.

| ID | Scenario | Required observation |
| --- | --- | --- |
| A01 | Start with closed case and no accessory connection | Fleet hosts remain visible; invitations work; no false connected seat |
| A02 | Complete enrollment while pods are unavailable | Both hosts retain membership and immediately list each other |
| A03 | A invites B, B cancels, A invites B again | Fresh invitation appears and can complete |
| A04 | A invites B, cancel or expire, then B invites A | No abandoned-key conflict; reverse invitation succeeds |
| A05 | Repeat A04 after restarting both daemons | No empty fleet survives to block enrollment |
| A06 | Enter a wrong code, then the correct code | Recoverable error and incremented attempt, followed by successful authentication |
| A07 | Enter five wrong codes; replay an older offer/proof | Bounded failure; no enrollment or key replacement |
| A08 | Opposing invitations overlap or old cancel packets arrive late | Roles remain isolated; no unauthenticated success; a fresh attempt can recover |
| A09 | Lose final enrollment acknowledgement or restart after one side saved | Verified membership survives; retry reconciles without foreign-fleet overwrite |
| A10 | Saved fleet is populated, inaccessible, or genuinely empty | Protect populated/inaccessible state; safely discard only confirmed empty membership state |
| A11 | Open case while both laptops have fresh activity and no known connection | Attempts do not cancel each other; available seats can both fill |
| A12 | Initial local/control/profile attempt fails transiently | Automatic bounded retry with visible attempt/deadline; no premature terminal error |
| A13 | A connected helper admits the other host, with and without a free slot | Correct retention decision; no needless helper disconnect; alongside outcome reported |
| A14 | Peer is busy, unavailable, or drops a routing acknowledgement | Visible retry or terminal reason; duplicate side effects prevented; no silent dead end |
| A15 | iPhone plays; move mouse on background laptop | Preparation may occur; iPhone playback continues |
| A16 | Start media on the background laptop | Ownership and output route change; both plugins identify the actual player |
| A17 | Switch playback repeatedly between laptops and iPhone | No feedback takeover loop; fresh intentional playback still works |
| A18 | Audio profile returns InProgress, then becomes ready | Control connection stays accurately reported; success replaces current failure |
| A19 | Explicit Apple reconnect intentionally drops local control | Progress is shown without a false unexpected-disconnect error |
| A20 | Receive stale roster, duplicate request, malformed fragment or replay after restart | No ghost owner/seat, repeated Bluetooth action, unbounded allocation, or secret leak |
| A21 | Activate AirPods after audio stack prefers AAC; repeat after reconnect | Highest available ranked stereo profile is selected every time |
| A22 | Acquisition fails with playback held; stream ID is subsequently reused | Original streams restore correctly; unrelated streams are untouched |
| A23 | Ear-removal settings and conversation awareness change during playback | Correct pause/resume and volume restoration without stealing remote audio |
| A24 | Record through the normal AirPods input during stereo playback | Nonzero correctly pitched audio; no second selectable source or headset-profile change |
| A25 | Run sustained recording with normal fleet discovery and playback | Measure missing packets, gaps, overflow and underflow; verify audible continuity, pitch and delivery rate |
| A26 | Mute, stop last recorder, disconnect, then reconnect | Capture and settings clean up; source availability follows readiness; playback remains correct |
| A27 | Battery absent, zero, below 10%, then refreshed; stale empty-case report | Accurate availability/freshness; UI receives no invented telemetry |
| A28 | BlueZ/audio server restart or suspend during an active operation | Old callbacks cannot finish new work; service recovers and UI converges |
| A29 | Plugin and browser observer communicate with the daemon | Commands, status parsing, code retries and activity events follow the interface contract |
| A30 | Two fleet hosts exchange initiator and helper roles | Enrollment in both directions, roster exchange, routing and alongside connections work |
| A31 | Direct reports, fleet reports and local stream activity disagree or expire | Each observation retains its provenance; one policy resolves the view; every emitted summary comes from the same snapshot |
| A32 | Retry, cancellation, late success and a new request interleave | Only the current operation owner transitions state; old completions cannot alter new work; no duplicate connection attempt or incompatible summary flags |

### 13.1 Plugin acceptance scenarios

| ID | Scenario | Required observation |
| --- | --- | --- |
| P01 | Close the case while the control link remains up, then observe disconnection | Connected seats remain until the drop; open-lid guidance then replaces the collapsed log without resizing the panel |
| P02 | A fast connection completes while the connector is animating | Diagram and artwork stay visible; connector completes smoothly; device enters once |
| P03 | Transfer the Omarchy seat repeatedly while iPhone keeps playing | Only the affected laptops move; iPhone stays seated; no duplicate disconnect or Apple-takeover events |
| P04 | Send duplicate/intermediate fleet snapshots during a switch or reconnect | No exit replay or teleport; progress label reflects the operation; log and seat animation start together |
| P05 | Local link drops while remote ownership changes or becomes unknown | Fresh fleet evidence updates the UI; stale ownership is not shown as confirmed; known devices remain in trays |
| P06 | Open enrollment without an accessory connection; enter a wrong code then correct it | Peer remains visible, dialog rearms once, progress is truthful, and success follows confirmed persistence |
| P07 | Open the panel or receive a startup playback snapshot while a remote device plays | No ownership claim; genuine activity can prepare, and a later explicit Play can claim |
| P08 | Restart the daemon/plugin, change a device name, or receive missing/stale batteries | Saved labels remain consistent; specific names survive generic reports; unknown and stale telemetry is not fabricated |

For A25, record at least 120 seconds per test condition, including playback and fleet traffic. Assess duration/pitch and listening quality as well as counters. A silent signal alone cannot distinguish intentional silence from missing audio; use a repeatable audible source and timestamp/packet evidence. Record discovery settings and keep them consistent across repeated runs.

Hardware validation means running these relevant cases on the real accessory, controllers, desktop audio stack and both hosts, with user listening checks where needed. Record which cases were run, what was measured, and what remains unverified. Do not infer all-SKU or all-controller support from the Pro 3 reference result.

## 14. Delivery and validation

Start with isolated test configuration and synthetic credentials. Deployment for hardware validation must be explicit, back up any installation being replaced, stop its daemon, and make rollback straightforward. Do not reset the user's fleet or copy secrets as a side effect of a build.

Deliver the Rust daemon and client, Quickshell/QML plugin with its manifest and assets, and required audio-session integration. Include build/install/rollback instructions, protocol and behavior tests, plugin state/animation tests, a requirement coverage table, a state-owner and status-field derivation map, and a hardware validation report. List missing features and unverified behavior explicitly.

For each measured release or build, record its revision, scope, and test environment along with:

- Handwritten production lines, test lines, integration scripts and generated/vendor code reported separately.
- Dependencies and any helper processes required at runtime.
- Build time, executable/distribution size, idle CPU and memory.
- Discovery, first connection, recovery and handoff latency under the same conditions.
- Playback profile/quality, microphone continuity and pitch, and radio activity.
- Number of independent state owners and implementations of each audio/control operation, with their ownership boundaries documented.

The implementation must be understandable, have clear state ownership, and satisfy the specified behavior. Evaluate maintainability alongside measured correctness and performance; line count alone is not a success criterion.

## 15. Protocol and interface evidence

Maintain synthetic fixtures for accessory setup and telemetry, fleet enrollment, presence, routing, roster exchange, local commands, and status snapshots. Record each fixture's protocol version, expected interpretation, and validation source. Use synthetic device identities and credentials; never capture live secrets in fixtures.

Document ambiguous encodings and timing assumptions before changing protocol behavior. Test both producers and consumers, including malformed input, duplicate delivery, partial messages, unknown values, and stale sessions. Distinguish behavior established by automated tests from behavior verified on hardware.
