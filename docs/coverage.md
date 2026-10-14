# Implementation coverage

Source audit: 2026-10-13. This report records implementation findings against the separate [build requirements](../REQUIREMENTS.md). The requirements describe intended finished behavior, not the current implementation status.

- **Wired:** a production path exists from input through decision to adapter or published status. This does not establish hardware reliability.
- **Partial:** part of the behavior is implemented, with an integration or validation gap.
- **Helper only:** isolated logic exists and has tests, but the corresponding production action is not connected.

| ID | State | Evidence and limits |
| --- | --- | --- |
| CON-1 | Partial | BlueZ, player, and audio observation are wired; accessory selection is loaded, listening preferences are not persisted |
| CON-2 | Wired | Fleet discovery remains independent of the accessory link; stowed state gates connection attempts |
| CON-3 | Wired | Handshake/features/notifications, battery and ear readiness, and connection-generation checks |
| CON-4 | Partial | Five attempts per normal series; continuing playback intent can reopen after failure cooldown; busy retries are separate |
| CON-5 | Partial | Snapshot stages/retries/reasons/deadlines exist; no lossless intermediate-event delivery |
| CON-6 | Wired | Profile-pending and busy handling are separate from control readiness |
| CON-7 | Partial | Interrupt/grace/recovery paths exist; complete hardware recovery matrix is not established |
| CON-8 | Partial | Key learning, storage and advertisement decoding are wired; in-memory readiness does not confirm durable writes; pre-key proximity matching is model-based |
| FLT-1 | Wired | Public/authenticated discovery and radio transport run without an accessory link |
| FLT-2 | Partial | Six digits and two-minute expiry; enrolled hosts can invite new members; fleet identity/key conflicts are rejected |
| FLT-3 | Partial | Five-attempt limit and corrected-code retry pass through fragmented enrollment messages; hardware validation remains separate |
| FLT-4 | Wired | Distinct enrollment phases are published |
| FLT-5 | Partial | Cancellation, reinvitation and simultaneous invites pass simulated transport tests; radio-loss/expiry hardware matrix remains open |
| FLT-6 | Partial | Tentative proposals do not immediately persist; explicit local reset and member removal are available through the CLI |
| FLT-7 | Partial | Secret Service write completion advances commit; dropped-final-ack/restart tests preserve fleet identity; the simulator does not exercise a real keyring |
| FLT-8 | Partial | 32-member cap and proposal/role validation exist; all enrollment acceptance scenarios pass, but they do not cover every membership topology |
| FLT-9 | Partial | Secret Service and locked-store retry exist; other load errors and individual key-write failures have limited state reporting |
| RTE-1 | Wired | Activity prepares a background connection or claims positively reported idle ownership; multiple eligible helpers cause it to be ignored |
| RTE-2 | Wired | MPRIS/browser play events can claim; stream-only routing requires direct local ownership |
| RTE-3 | Wired | Shared laptop occupancy and alongside outcomes are represented |
| RTE-4 | Wired | Authenticated helper admission chooses retention from peer/ownership evidence |
| RTE-5 | Wired | Transaction identity, duplicate replies, admission and completion are separate |
| RTE-6 | Wired | Routing deadlines/retransmissions and local connection retries are separate |
| RTE-7 | Wired | Authenticated roster freshness, accessory/session checks and replay watermarks |
| RTE-8 | Wired | Alongside outcome exists; panel animation synchronization needs separate validation |
| RTE-9 | Partial | Known MPRIS players are paused on yield; hidden browser media can continue on speakers |
| RTE-10 | Wired | Apple reconnect has peer-release, pause, drop, rejoin and return phases with deadlines |
| AUD-1 | Wired | Fixed codec ranking and profile application; ranking values are not measured negotiated bitrates |
| AUD-2 | Partial | Capture does not select headset profiles; session-manager prevention requires WirePlumber hooks |
| AUD-3 | Wired | Ordered mute/recovery/default/move/restore through the PulseAudio interface |
| AUD-4 | Partial | Timed takeover holds and generation-checked restoration; shutdown does not explicitly drain restoration work |
| AUD-5 | Wired | Connection readiness and observed audio-route completion have separate state |
| AUD-6 | Partial | Setting writes exist; only noise-mode confirmation is wired; preferences are in memory |
| AUD-7 | Helper only | Debounce and pause ownership tested; returned ear actions are discarded and regular timer does not advance debounce |
| AUD-8 | Partial | User reports volume controls working on hardware. The daemon's rounding and duck-save/restore helpers lack production callers; this does not establish a failure of system/accessory volume controls. Conversation ducking and restoration remain separately unverified |
| MIC-1 | Partial | Source demand/mute wired; source is not explicitly hidden; normal-input integration requires separately installed hooks |
| MIC-2 | Wired | Control-channel frame parsing, FFmpeg AAC-ELD decode, gain and queued local PCM; audible quality not established by tests |
| MIC-3 | Wired | One format table for three Pro 3 SKUs; not evidence of hardware validation for every SKU |
| MIC-4 | Wired | 65,536-sample queue, 200 ms reserve, whole-block drops and silence/re-prime on underrun |
| MIC-5 | Partial | Packet/decode/queue counters exist; no complete end-to-end loss attribution |
| MIC-6 | Wired | Capture leaves discovery running; daemon does not configure BlueZ scan defaults |
| MIC-7 | Partial | Capture/source retries exist; conversation-awareness restoration depends on unwired confirmation input |
| MIC-8 | Wired | PCM stays in local delivery; no daemon recording or phone relay |
| TEL-1 | Partial | Fleet and Apple names saved/published; no complete persisted accessory-identity cache |
| TEL-2 | Wired | Missing batteries omitted, zero preserved; last-known values can outlive internal freshness |
| TEL-3 | Wired | Lid freshness/last-known distinction and ownership invalidation; model-based fallback before keys |
| TEL-4 | Partial | Structured snapshots and outcomes; UI history/animations are separate and intermediate states may be missed |

## Validation results

Release validation update: 2026-10-13. These results are from the full local suite after the pairing, enrollment-dialog, and icon-alignment changes. The implementation table above remains the earlier source audit.

| Check | Result |
| --- | --- |
| `cargo test -p fleetpods --lib --bins` | 263 unit tests passed; both binary targets build |
| `cargo xtask test` | All 32 acceptance scenarios passed |
| `cargo clippy -p fleetpods --all-targets --features accept -- -D warnings` | Passed |
| `cargo clippy -p xtask --all-targets -- -D warnings` | Passed |
| `cargo fmt --check` | Passed |
| `deno run --allow-read plugin/tests/model.test.js` | All model checks passed |
| `deno run --allow-read plugin/tests/replay.test.js` | Both recorded sessions passed log and seat checks |
| Qt 6 QML tests, offscreen software rendering | 158 checks passed, including setup/cleanup and data rows; none failed or skipped |
| `cargo build --release --bins --locked` | Passed |
| `omarchy plugin validate .` | Passed |
| `bash -n setup` | Passed |

The first formatting check found drift in recent Rust changes. `cargo fmt` corrected it before the successful formatting, Clippy, unit, acceptance, and release-build runs.

The acceptance failures were traced to the simulator delivering bare enrollment JSON without the sender
information carried by advertisement fragments, ignoring actual advertisements, and forwarding observation
effects that the production adapter does not transmit. The simulator now delivers paced advertisement frames,
advances its clock through fragmented messages, and reports keyring/disconnect completion. Routing scenarios
start with enrolled peers and use activity for admission. A07 replays a captured enrollment offer, A09 drops
actual final-ack fragments, A14 drops admission frames, and A20 rejects expired advertisements before accepting
fresh reports. A30 allows for the public/private presence cadence.

The replay expectations were reviewed against the recorded inputs and current behavior. Each log change is
annotated beside the unchanged recordings. The review also fixed a waiting-to-connecting row split and
duplicate Apple reconnect progress. QML checks cover the solid connector during a fleet handoff and artwork
source selection during case transitions. The test runner no longer embeds a checkout path that breaks after
moving the workspace.

The table records local results. CI includes native build dependencies, Rust checks, the release build,
acceptance scenarios, plugin model/replay checks, installer syntax validation, and the QML suite in an Arch
Linux container. Hosted results are recorded in [GitHub Actions](https://github.com/DanMacDonald/fleetpods/actions/workflows/ci.yml).
No live keyring or Bluetooth service was changed during this automated validation pass. The separate
hardware reports below document their deliberate setup changes and observations.

Passing simulation does not establish Linux adapter wiring or hardware reliability. The world still supplies
synthetic accessory replies, successful platform callbacks and simplified audio/capture behavior. In particular,
the A23 ear/volume and A25 microphone scenarios exercise helper logic, not a complete live hardware path.
The two older UI recordings include failures and expired evidence. They are regression inputs, not proof that
every real handoff succeeds.

This audit does not certify full compliance with the build requirements or completion of release testing.

## Subsequent user observations

The [2026-10-13 enrollment hardware validation](enrollment-validation.md) subsequently passed decline in both directions, real invitation expiry, wrong-code recovery and successful joining in both directions, and membership persistence after restarting each daemon. It records the tested scope separately from the earlier source audit and simulated acceptance results.

The [Gram first-run validation](first-run-validation.md) subsequently covered a clean source install, manual Bluetooth pairing, bond persistence through daemon restart and case closure, manual fleet joining with wrong-code recovery, and Apple-seat Reconnect. Reconnect recovered from a transient retry; the report records that limitation.

The [two-laptop hardware regression](hardware-regression.md) records repeated Reconnect, idle activity, browser playback, playing-peer protection, high-resolution microphone checks, and real suspend/resume on both laptops. It exposed a suspend callback-order bug, which was corrected and covered by two new tests; the subsequent unit run passed all 265 tests. The report records the observed reconnect, stream-only playback, and microphone startup limitations.

- Volume controls worked in the user's hardware test. The exact input path and five-percent step behavior were not identified, and this observation does not independently verify conversation-awareness ducking/restoration.
- Clicking the AirPods to pause playback on Gram was reported to also start playback on a connected iPhone. The observation has not been correlated with button, ownership, player, and fleet events. It is not yet attributed to the daemon, the accessory, or iPhone behavior; no corrective takeover or pause policy has been added.


## Switching validation update, 2026-10-14

The [switching validation](switching-validation-2026-10-14.md) records the current 285 passing unit tests, 32 passing acceptance scenarios, 177 passing QML checks, and the model, replay, formatting, Clippy and release-build checks. It also records the corrected background reconnect race and live two-laptop results: six standard Reconnect cycles, four deliberate race checks, six browser takeover/protection/pause cycles, and eight idle activity transfers. The source audit above remains dated 2026-10-13; this validation update does not replace it.
