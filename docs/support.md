# Support and limitations

## Tested hardware

FleetPods 0.1.0 currently targets AirPods Pro 3 on Omarchy.

| Device or platform | Validation |
| --- | --- |
| AirPods Pro 3 | Pairing, fleet joining, switching, connection display, battery, and high-resolution microphone capture tested |
| Dell XPS 16 DA16260 | Tested on an Omarchy 4.0 development build with shell plugin support |
| LG Gram 17Z990 | Tested on Omarchy 4.0.4, including a fresh install and first pairing |
| Other Omarchy computers | Not yet hardware-tested; require the dependencies and Bluetooth capabilities in the [installation guide](installation.md) |
| Other AirPods Pro models, standard AirPods, and AirPods Max | Not yet validated for release support; some discovery and controls may work |
| macOS and iOS | Apple connections can appear in the plugin. These devices do not run the FleetPods daemon or join its fleet |

The [hardware regression report](hardware-regression.md), [first-run report](first-run-validation.md), and [enrollment report](enrollment-validation.md) record the tested scope. A minimum compatible Omarchy version has not been established; the `omarchy plugin` commands must be available.

## Switching and reconnecting

Nearby setup discovery broadcasts the computer's name, Bluetooth address, and whether it already has a fleet. Fleet routing and connection-roster updates are encrypted and authenticated with the shared fleet key. The setup discovery hint does not authorize a device to join.

Pressing Play in a supported media player requests ownership. Browser Play/Pause was tested through desktop media-player events. When both Omarchy laptops are connected and idle, fresh desktop activity can transfer ownership without dropping their Bluetooth connections. Activity does not interrupt a playing peer.

Apps that only create an audio stream, without media-player Play events, cannot initiate automatic takeover today. They can play through an already-owned connection. Take the Omarchy seat in FleetPods, or run `fleetpods connect`, before starting that playback.

AirPods have two connections. Apple switching still depends on which Apple device retains a connection; selecting the AirPods manually in Apple's Bluetooth menu can be necessary. **Reconnect** on the Apple seat briefly releases the Omarchy connections, then reconnects the Omarchy computer that initiated it. It keeps the saved Bluetooth pairing.

Reconnect can take several seconds and may retry. All six repeated hardware checks completed, with one recovered transport failure taking about 15 seconds. This release does not promise instant or retry-free reconnects.

If playback goes silent while this computer still owns the AirPods, `fleetpods audio reset` briefly reopens its audio output without changing pairing or fleet membership. The daemon also does this automatically when an unmuted playing stream remains on an idle, suspended or failed output for three seconds. Recovery has a 30-second cooldown and a maximum of three attempts per sustained failure. It skips sleep, handoffs, pauses and muted playback. An output that incorrectly reports itself as running can still need the command; the daemon cannot verify what the listener hears. Reset completion confirms the output commands succeeded, not that sound reached the buds.

## Microphone and case display

AirPods Pro 3 microphone capture was tested at 64 kHz mono while retaining stereo playback. Capture starts with roughly half a second of silence before decoded audio arrives. Real call quality, long recording sessions, and microphone support for other models still need validation.

Before pairing, case artwork and instructions come from nearby advertisements. With multiple pairs nearby, an active pairing report takes priority; otherwise an open case with both buds docked takes priority, with signal strength breaking ties. Signal strength is a hint, not a distance measurement. The device you select in the Bluetooth panel determines what is paired.

FleetPods pauses its scan during first pairing to let the Bluetooth panel complete it. Live case and light updates may pause too. Pairing-mode detection is currently verified for AirPods Pro 3, and battery indicators stay hidden until pairing.

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
