# Architecture

One process. One composition root. Each fact has one module that changes it. The status file is a picture of those modules, taken once, and nothing reads the picture back.

There is no platform-trait layer. Linux is the first delivery. A later macOS or Android participant is a separate program that speaks the fleet wire, not a generic adapter inside this crate.

The Quickshell panel lives in `plugin/` at the repository root, next to `src/`. Omarchy loads `plugin/Panel.qml`. That QML reads the status file and sends socket commands. It does not call into `pods`, `fleet`, or `audio`. `src/panel/` is only the daemon's writer of those two compatibility surfaces.

## Modules

| Module | Changes | Leaves alone |
| --- | --- | --- |
| `types` | Nothing. Shared words: identity, time, evidence, model capabilities | Decisions that change over time |
| `pods::link` | BlueZ observation, control socket, setup stage, the local retry, connection generation | A second recovery loop from the fleet |
| `pods::reports` | Decoded telemetry, confirmed settings, direct ownership and source reports | The user's saved preference |
| `pods::air` | Nothing. Proximity-pairing parse, address resolution, and the encrypted battery block | Which advertisement belongs to this accessory |
| `pods::prefs` | Selected accessory and saved preferences | Accessory-confirmed settings |
| `fleet::nearby` | Nearby hosts and their expiry | Membership |
| `fleet::roster` | Authenticated roster reports, bound to reporter, accessory, session, and sequence | Direct accessory reports |
| `fleet::members` | Fleet records and accessory keys | The invitation in progress |
| `fleet::enroll` | One invitation: role, attempt, phase, expiry, tentative keys | A durable fleet |
| `fleet::activity` | The durable preferred connector, authenticated activity gossip, and one wake request per availability cycle | Playback ownership, Bluetooth retries, or a fallback election |
| `fleet::route` | Handoff intent and one admission transaction | The local Bluetooth retry |
| `fleet::wire` | Service UUID and the enrollment context string | Enrollment outcome |
| `audio::play` | Profile choice, player observations, ear pause, volume ducking | Stream mute/volume holds |
| `audio::route` | Routing, original stream mute/volume state, handoff drain, recovery and ordered restoration work | Seat ownership and opening PipeWire |
| `audio::mic` | Capture while the hidden source is streaming and not muted, the restart budget, and the verified format table | Playback profile policy |
| `audio::seat` | Nothing. Who is playing, read from the pods and the fleet | The reports it reads |
| `panel::status` | Nothing. The JSON document | Decisions |
| `panel::ipc` | Nothing. Framing and verb parsing | What a verb means |
| `daemon` | Nothing of its own. One `step` sends each input to one module | A second copy of that module's fields |

`linux` is the edge: hostname, random bytes, signals, the two filesystem locations, BlueZ, the Secret Service, and PipeWire. `fleetpodsd` calls `Daemon::step` and performs the effects. The daemon chooses each version-1 letter fragment and each version-2 public presence frame. A letter takes the next slot. Presence airs for 500 ms every 5 s. An open fleet sends version 4, sealed with the fleet key and the routing session. A roster snapshot alternates with presence on the same cadence. A routing packet takes the idle slot before either of them. Discovery stays up while this host has a controller address; releasing the advertisement leaves that scan running. A proximity-pairing advertisement is a separate input. The daemon accepts it when the stored resolving key matches, or when no key is stored and the model is recognized. Once the control link is ready, bud battery stays with that link; the lid and the case battery still follow the advertisement. The BlueZ thread only writes the service data, forwards Apple manufacturer data, and starts or stops the scan it was given. It also forwards logind sleep, bluetoothd leaving or returning, and the controller's power. The audio thread reports when its server context dies. The hidden source `fleetpods.hires-microphone` is published while a Pro 3 link is ready. Capture starts when that source is streaming and not muted. A format other than 32-bit float, mono, at the delivery rate is not played. A failed stream is opened again after five seconds. The WirePlumber hook in `wireplumber/` keeps playback on A2DP and is not installed. Those inputs run the existing interrupt path: a link that was up is dropped, the ownership report is cleared, and the playback claim stays. The radio stays quiet until the machine can use the controller again. A return waits two seconds before that link is opened again, and a second interruption during the wait does not cancel it. The BlueZ thread does not attach the control socket during that wait. A disconnect the user asked for is not opened again. A control-socket drop clears the local report and leaves the fleet transaction alone. The control socket is polled on its own with the bus; a BlueZ call does not stop that read. Adapters do not grow a second connection state machine.

## One step

`Daemon::step(input, now)` is the only way the platform changes the daemon. The clock on that call is the clock for every decision inside it. The call returns effects: open this link generation, send these bytes, advertise one chosen frame, release that advertisement, keep discovery running, announce presence, restore these streams, or write this fleet to the keyring. The adapter runs the effects. It does not pick a profile, start a route, decide who owns playback, or choose which fragment is on the air.

A play asks for the link and records when it asked. When that link is already up, or when accessory reports finish setup, the daemon writes the local accessory report if no newer remote report has arrived. `PodsConfirmed` is the platform's report that someone else owns playback. A remote report that arrives after the play wins. A remote report that was already stored is older, even at the same millisecond. Connection decisions match `Stage`, and the status file prints `Stage::wire`.

The acceptance world answers an open link the same way the BlueZ thread does: BlueZ up, control socket up, then the handshake, feature, and notification bytes. A packet is either a setup reply or a report. Reports parse the same way before and after the link is ready. `ConnectAudio`, `Disconnect`, and `WriteControl` name the accessory. `ConnectAudio` also carries the profile the daemon selected. A play claims the seat. PipeWire reports a sink graph when it changes, and `Tick` re-plans that graph on the daemon clock. The daemon mutes managed streams on the current default, then selects the accessory sink, then moves those streams. Each move carries the stream serial. Streams already on another sink stay there. MPRIS is a signal, not a poll: the startup picture does not claim, and one player pausing does not clear another that is still playing. Membership is applied only after `KeyringStored`. A refused write is `KeyringFailed`, and the fleet stays uncommitted.

Enrollment letters are bytes from the moment they leave the daemon (`pf1` plus one JSON object, encoded in `fleet::postcard`). The platform never sees the in-memory `Letter` enum. That frame is not the C++ advertisement; A30 still needs captured fixtures. SPAKE2 itself lives in `fleet::crypto`, and the conversation that uses it lives in `handshake`.

The acceptance world is a clock, a mailbox, and a log of those effects. Each scenario has its own world. The world answers `OpenLink` with `LinkOpened` or `LinkFailed` and forwards bytes. It does not finish a link on the daemon's behalf.

## Remembered connector

`fleet::activity::Preference` owns one record scoped to the fleet ID and paired accessory: owner address and activity time. Physical desktop activity, a new accepted Play, and explicit Connect/HandoffClaim update it, including while the pods are stowed. Startup player snapshots, case advertisements, heartbeats, and reconnects do not create activity. Equal timestamps use address bytes as a deterministic tie break; subsequent local input advances past a just-heard timestamp. Preference does not expire when its owner sleeps or leaves discovery.

Version 9 advertisements gossip this record independently of roster/audio ownership. The 63-byte frame is `9 || seal_fleet("fleetpods-activity-v1", fleet_id, fleet_key, body)`, with a 34-byte body: relay address (6), accessory (6), owner (6), activity Unix milliseconds (8, big endian), envelope Unix milliseconds (8, big endian). Both relay and owner must be current fleet members (or the local device); wrong-accessory, unauthenticated, expired-envelope, and excessive-future-time frames are rejected. Envelopes expire after 15 seconds with the existing 5-second skew allowance; the remembered activity does not expire. Relaying a record refreshes only the envelope. Refreshes use a bounded radio slot every three seconds; new activity can be offered immediately. Existing presence and roster formats stay compatible.

The Linux adapter atomically saves changes in `activity.preference` beside the status file under the user's state directory and restores them on startup. Resetting the fleet, changing the accessory, or removing the preferred member clears the record. Status exposes `fleet_preference` for diagnosis. An enrolled fleet with no valid record waits for fresh activity or explicit Connect.

When the case opens or a pod becomes available, the preferred device waits two seconds, then asks the existing `pods::link::Connection` to connect in the background. The preference module never retries Bluetooth itself. Every automatic open and unsolicited attach is gated by the preference; a newer remote preference cancels an opening attempt and releases any held speaker audio. An established connection is not evicted merely by a preference change. Existing Apple playback protection, pairing holds, user disconnects, and explicit Apple reconnect transactions still apply. Availability alone never claims audio.

All fleet daemons must be upgraded for this policy: older versions ignore version 9 and retain their old reconnect behavior. BLE gossip provides convergence when packets arrive, not distributed consensus during a radio partition. A device that has not received newer activity can temporarily have an outdated preference; there is no timeout-based fallback to a known nonpreferred host. Validate cross-device behavior on hardware as well as in the two-daemon tests.

## Audio handoffs

`audio::route::Router` owns saved stream state from acquisition through routing,
handoff drain and recovery. The daemon supplies claim/release intent, graph
observations and the clock. It drains ordered work once per step after all
ownership and recovery decisions. Mutes execute ahead of player pauses and
control writes. Recovery can cancel a queued restore before it reaches Linux;
a new claim inherits a handoff's originals before reading a muted graph. An
audio-server restart discards saved serials and every queued mute or restore.
`Audio` keeps player observations, not a second copy of routing holds.

## Artwork ownership

`Model` interprets physical pod placement and `Service` retains that evidence.
Neither schedules illustration changes. `AirPodsIcon` owns confirmation of the
last departure, pod motion, empty-case hold and exit, standalone entrance,
interruption, and renderer lifetime. Hidden artwork settles without replaying
old transitions. The icon publishes connector positions so `SeatsView` does not
interpret animation flags. Qt's scene-graph animator still moves the entrance
texture independently of shading work.

`MeshArtwork` owns the case and standalone frames, display-scale clearance,
palette, camera and resting connector anchors. Popup and preview both use it.
`DotPods` paints coverage separately from surface shading; the grid, mesh and
light/dark rasterizers remain internal implementations. Framing tests capture
the actual artwork at the same interface used by those callers.

## Rules that the types enforce

Unknown, absent, stale, and fresh are different enum variants. A missing battery is not level 0. A BlueZ report that has not arrived is not "disconnected"; the JSON layer is the only place that collapses that into the legacy `false`.

A connection attempt is one enum. Retry-scheduled and terminally failed cannot both be true. Enrollment phases and connection phases are different types. An enrollment attempt's code is a SPAKE2 password held by `fleet::enroll`. That module does not compare codes. A repeated delivery of the same attempt does not burn another try.

Host identity, accessory address, operation id, invitation id, and session id are different types. A callback carries the connection generation it started with. A newer generation ignores it.

Tentative fleet keys live on the enrollment attempt. Cancellation, expiry, authentication failure, and restart drop them. Membership changes only when `Daemon::persist_enrollment` is called after a keyring write has returned. An unreadable keyring is not an empty fleet. A different fleet id does not overwrite an open one.

## How a fact is added

1. Put the value in the module that already changes that kind of fact.
2. If it is a new kind of fact, add one file under `pods`, `fleet`, or `audio`, and one field on `Daemon`.
3. Feed it from `Daemon::step`. Do not let a second module store a writable copy.
4. If the panel needs it, project it in `panel::status` and name the source in `docs/status-fields.md`. The QML reads that field. It does not gain a second copy of the fact.

Do not add an event bus, an actor framework, or a trait per operating system to do this.

## Clocks

`pods`, `fleet`, and `audio` use `MonoMs` for local deadlines. Cross-device activity ordering and authenticated envelope timestamps use `UnixMs`; the preference survives reboot. The status file uses Unix milliseconds. `Clock` is a paired reading taken by the runtime, and it is the only conversion.

## What is deliberately absent

- A generic `Platform` trait.
- A second implementation of profile selection, stream hold, or the local connect loop.
- Serde on the state structs. The wire format is built in `panel::status` so a JSON key cannot become an internal field by accident.
- A claim that the `pf1` postcard matches the C++ daemon's advertisement bytes. The context string and service UUID stay fixed in `fleet::wire`.
- Microphone PCM, packet bytes copied from the AGPL decoder, and any recording of the user.
