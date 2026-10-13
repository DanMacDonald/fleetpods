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
