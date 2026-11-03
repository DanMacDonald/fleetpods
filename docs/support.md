# Support and limitations

## Tested hardware

FleetPods 0.1.0 currently targets AirPods Pro 3 on Omarchy.

| Device or platform | Validation |
| --- | --- |
| AirPods Pro 3 | Pairing, fleet joining, switching, connection display, battery, and high-resolution microphone capture tested |
| Dell XPS 16 DA16260 | Tested on an Omarchy 4.0 development build with shell plugin support |
| LG Gram 17Z990 | Tested on Omarchy 4.0.4, including a fresh install and first pairing |
| Other Omarchy computers | Not yet hardware-tested; require the dependencies and Bluetooth capabilities in the [installation guide](installation.md) |
| AirPods Max 2 (A3454) | Head-wear advertisements, control-report removal/re-wear pause/resume, and high-resolution microphone capture tested on XPS; desktop input meter and short voice recording/playback checked; long calls untested |
| Other AirPods Pro models, standard AirPods, and earlier AirPods Max | Not yet validated for release support; some discovery and controls may work |
| macOS and iOS | Apple connections can appear in the plugin. These devices do not run the FleetPods daemon or join its fleet |

The [hardware regression report](hardware-regression.md), [first-run report](first-run-validation.md), and [enrollment report](enrollment-validation.md) record the tested scope. A minimum compatible Omarchy version has not been established; the `omarchy plugin` commands must be available.

## Switching and reconnecting

Nearby setup discovery broadcasts the computer's name, Bluetooth address, and whether it already has a fleet. Fleet routing and connection-roster updates are encrypted and authenticated with the shared fleet key. The setup discovery hint does not authorize a device to join.

Pressing Play in a supported media player requests ownership. Browser Play/Pause was tested through desktop media-player events. When both Omarchy laptops are connected and idle, fresh desktop activity can transfer ownership without dropping their Bluetooth connections. Activity does not interrupt a playing peer.

Apps that only create an audio stream, without media-player Play events, cannot initiate automatic takeover today. They can play through an already-owned connection. Take the Omarchy seat in FleetPods, or run `fleetpods connect`, before starting that playback.

AirPods have two connections. Apple switching still depends on which Apple device retains a connection; selecting the AirPods manually in Apple's Bluetooth menu can be necessary. **Reconnect** on the Apple seat briefly releases the Omarchy connections, then reconnects the Omarchy computer that initiated it. It keeps the saved Bluetooth pairing.

Reconnect can take several seconds and may retry. All six repeated hardware checks completed, with one recovered transport failure taking about 15 seconds. This release does not promise instant or retry-free reconnects.

If playback goes silent while this computer still owns the AirPods, `fleetpods audio reset` briefly reopens its audio output without changing pairing or fleet membership. The daemon also does this automatically when an unmuted playing stream remains on an idle, suspended or failed output for three seconds. Recovery has a 30-second cooldown and a maximum of three attempts per sustained failure. It skips sleep, handoffs, pauses and muted playback. An output that incorrectly reports itself as running can still need the command; the daemon cannot verify what the listener hears. Reset completion confirms the output commands succeeded, not that sound reached the buds.

On 2026-11-03, Max 2 control reports triggered removal pause and re-wear resume on
XPS. Reducing the headset removal debounce from 400 ms to 100 ms shortened the
observed report-to-Pause interval from 403–459 ms to 133 ms; re-wear still uses
400 ms. The user reported a slightly faster pause and clean resume in the next
trial. Earlier right-channel popping lasting 30–40 seconds did not recur in that
trial or in manual pause/removal/re-wear trials with FleetPods stopped. Its cause
is not established; the microphone was idle and observed PipeWire error counters
were zero. A later recurrence began during uninterrupted XPS AAC playback;
both microphones were idle, ownership stayed on XPS, and PipeWire again reported
zero errors. Popping continued after stopping both FleetPods daemons with playback
uninterrupted, then settled. This does not establish whether earlier setup caused
it, but removal/resume and ongoing daemon traffic are not required for it to
continue. Replaying the affected passage reproduced pops at the same timestamps,
which points toward the source recording or content-dependent playback; a speaker
comparison is still needed. No codec or buffer workaround was applied.

## Microphone and case display

High-resolution microphone capture is enabled for AirPods Pro 3 (A3063/A3064/A3065), Pro 2 Lightning (A2698), and Max 2 (A3454), using the same AAC-ELD format and 64 kHz mono source while retaining stereo playback. Older Max models keep the stock Bluetooth input. Select the accessory's name as the input in the recording or call application.

On 2026-11-03, an XPS Max 2 test delivered 2,640 frames over 20 seconds with zero malformed packets, decoder errors or queue drops; A2DP playback stayed active. The decoded source produced nonzero audio, and the user confirmed their voice appeared in the desktop input meter. A subsequent 20-second voice recording was played through the Max, and the user reported clear pitch. This adds a subjective listening check; it is not a calibrated pitch measurement. The source reported 24,917 missing samples during the test (about 0.39 seconds at 64 kHz); startup is expected to include roughly half a second of silence. Long call sessions and sustained capture reliability remain untested. Pro 2's delivery rate was previously checked with an 880 Hz tone (see [hardware notes](multi-accessory.md#hardware-notes-2026-11-01)).

The microphone adapter tracks its PipeWire client links so capture stops when the last recorder or input meter closes, including during ongoing stereo playback. A hardware check with simultaneous 64 kHz and 48 kHz clients confirmed that closing one preserves capture, closing the last returns to idle with a stable frame count, and reopening starts capture again without decoder errors.

Before pairing, case artwork and instructions come from nearby advertisements. With multiple pairs nearby, an active pairing report takes priority; otherwise an open case with both buds docked takes priority, with signal strength breaking ties. Signal strength is a hint, not a distance measurement. The device you select in the Bluetooth panel determines what is paired.

FleetPods pauses its scan during first pairing to let the Bluetooth panel complete it. It also yields its own discovery and fleet advertisement whenever BlueZ shows another classic-audio device connecting before its bond is saved, even on a computer that already has AirPods and even when that device does not use the AirPods Pro 3 pairing advertisement. FleetPods does not disconnect or take over that device. Live case and light updates may pause too. Pairing-mode detection is currently verified for AirPods Pro 3, and battery indicators stay hidden until pairing.

## Troubleshooting

Check the daemon and recent logs first:

```sh
systemctl --user status fleetpods.service --no-pager
journalctl --user -u fleetpods.service --since "10 minutes ago" --no-pager
fleetpods status
```

- **No bar icon:** Enable it with `omarchy plugin enable io.github.danmacdonald.fleetpods`. After an upgrade, `omarchy restart shell` reloads cached plugin components. The plugin can also hide its icon when disconnected, according to its settings.
- **Bluetooth connected but an empty seat:** The AirPods control connection may still be opening. Wait for setup to finish. If it stays pending, keep the buds available and inspect the logs; a disconnected control session does not by itself mean pairing was lost.
- **Bluetooth restarts while audio plays on the laptop speakers:** FleetPods preserves pairing while BlueZ restores its device list. An empty startup inventory is not an unpair and must not pause, mute, or reroute speaker playback when the pods are unavailable.
- **Fleet joining or reset reports a keyring error:** Unlock the login keyring and retry. Joining needs access to save membership. Reset reports success only after saved membership is deleted.
- **Waiting for keyring after reboot:** FleetPods retries locked or failed startup reads every five seconds and restores saved membership automatically when the keyring becomes available. If the message persists, unlock your login keyring. Rejoining the fleet is unnecessary.
- **Wrong fleet code:** Correct it in the same invitation while it remains valid. If it expires, start a new invitation. Joining another fleet may require confirming that you want to leave the current one.
- **No microphone input:** Confirm the daemon is connected, select the AirPods input in the application, and check that WirePlumber is running. If capture stops, close and reopen the application's input.

For a bug report, include the FleetPods and Omarchy versions, AirPods model, what you did, what each laptop showed, and the relevant logs. Remove enrollment codes and personal device identifiers from shared output. Never include keyring secrets. The [coverage report](coverage.md) separates automated checks from hardware validation.
