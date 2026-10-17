# Status field sources

`status.json` is one snapshot. The panel's wire names are kept. A field below is either copied from one module or derived by `audio::seat`. It is not a second writable copy.

Legacy sentinels exist only in this projection. `None` for BlueZ becomes JSON `false` because the old bool cannot say unknown. `pods::link` still stores `None`.

| Field | Source |
| --- | --- |
| `schema_version` | Constant `1` |
| `device_name`, `model_name`, `model_int`, `model_number`, `is_pro_series`, `is_headset` | Accessory identity |
| `supports_*` | Model capability table |
| `local_name`, `local_address` | This host, learned at runtime |
| `connected` | Control socket is observed up. Not setup-ready, not BlueZ |
| `bluez_connected` | BlueZ reported up. Unknown and down both become `false` here |
| `bluetooth_paired` | The selected accessory has a local BlueZ pairing. Removing the device or clearing both `Paired` and `Bonded` returns this to `false`, cancels connection attempts, and restores pairing guidance. Saved accessory keys alone do not count as Bluetooth pairing |
| `bluetooth_bonded` | BlueZ confirms saved Bluetooth pairing keys for the selected accessory. `null` means not yet observed; separate from `Trusted`, the current connection, and accessory keys. An explicit `false` blocks Apple-seat Reconnect and shows pairing guidance without discarding the current connection |
| `ear_reported` | An ear placement has been reported |
| `pairing_keys_ready` | Both accessory keys are stored. Missing stays `false`, not omitted |
| `handoff_ready` | Socket up and connection stage `ready` |
| `control_connection.stage`, `.reason`, `.failed_stage`, `.recovering`, `.retry`, `.retry_limit` | `pods::link`. Retry and terminal failure are different stages |
| `control_connection.next_retry_ms` | Connection deadline, Unix ms. Omitted when no deadline is armed |
| `control_connection.peer_reconnect`, `.peer_reconnect_error` | Apple reconnect flag on `pods::link` |
| `noise_mode` | Accessory-confirmed mode, or `-1`. The saved preference is not copied in |
| `adaptive_noise_level` | Accessory-confirmed level. Omitted when unknown. The panel then reads `0` |
| `conversational_awareness`, `one_bud_anc_mode` | Confirmed `true`, otherwise `false`. Off and unknown are the same on the wire |
| `ear_detection_behavior` | Confirmed accessory value when one exists, otherwise the saved preference |
| `left`, `right` | Known battery reports and independently observed placement (`in_ear`, `in_case`). Unpaired proximity observations can provide placement with `available: true` but no `level` or `charging`; missing battery fields remain unknown. Omitted when neither is known |
| `case`, `headset` | Known battery reports, including a real `0`. Omitted when unknown. Not emitted as `available: false` |
| `lid_state` | Fresh lid report, otherwise `2`. A stale report does not stay "live" |
| `last_known_lid_state` | Last lid value actually observed, otherwise `2` |
| `lid_updated_ms` | Time of the latest lid observation. `0` if none. A missing advertisement does not invent a lid |
| `last_advertisement_ms` | Time of the latest accepted proximity advertisement. `0` if none. Independent of whether that frame carried a lid |
| `ble_connection_state` | Connection byte from that advertisement, or `255` until one is accepted |
| `case_confirmed_docked` | Both buds freshly reported in the case |
| `nearby_airpods` | Separate unpaired advertiser records: address, optional name/RSSI, model, observed placement/lid, report age and selected setup candidate. Equal names and models never merge records. Private address rotation cannot be resolved without pairing keys |
| `pairing_candidates` | Number of advertisers with fresh, recognized pairing-mode evidence during unpaired setup. Multiple candidates prompt selection in the Bluetooth panel; RSSI only chooses setup guidance and never pairs a device |
| `pairing_mode` | Fresh Pro 3 pairing-mode advertisement during unpaired setup. Normal placement reports can interleave without clearing it. Clears on a closed-lid report, local connection, or after 3 seconds without another pairing advertisement. Pairing payloads are not decoded as placement or battery data |
| `waiting_for_playback` | Connection stage is `waiting` |
| `microphone.phase` | `down`, `idle`, `capturing`, or `needs_reopen` |
| `microphone.frames`, `.malformed`, `.max_gap_ms` | Counts from `0x58` audio packets while capture is on. The AAC bytes are not stored |
| `microphone.decode_errors`, `.dropped`, `.missing` | Frames the decoder rejected, blocks the queue refused, and samples replaced with silence while capture is delivering. Silence while capture is off is not counted |
| `reported_ownership` | Direct ownership report: `1` local, `0` remote, `-1` unknown. Stale values are still projected, with their timestamp |
| `audio_source_local`, `audio_source_type` | Direct source report, same rule. Not filled from `audio::seat` |
| `audio_owner` | `audio::seat`: fresh direct reports, then a fresh fleet roster, then the last direct holder from this control session. An aged source category alone does not mean playing; current playback observations supply that state. Disconnect clears the remembered direct holder |
| `audio_output_health.state`, `.resets`, `.last_reset_ok` | Raw Pulse sink state (`0` running, `1` idle, `2` suspended, negative transient/error values), number of output resets requested in this daemon process, and whether the last suspend/resume commands completed. `null` means unobserved or pending. A successful reset does not confirm audible playback |
| `remote_playing` | Resolved holder is remote and playing |
| `remote_source_address` | Direct remote-address report. Omitted when unknown |
| `roster_audio_owner` | Resolved holder's address, falling back to the fresh fleet roster when the resolved holder has no address |
| `ownership_updated_ms`, `source_updated_ms` | Timestamps of those direct reports |
| `peer_list_updated_ms` | The pods' own peer list time once the link is ready; `0` while the socket is open but setup is unfinished; otherwise the fleet roster `sent` time, else `0` |
| `ownership_intent` | Routing intent: `none`, `claim`, `release`. Desktop activity does not set `claim` |
| `handoff_result` | Routing handoff result. `none` until a control write exists |
| `remote_peer_count` | `-1` until the control socket is up and a fresh roster names peers |
| `connected_peers` | Those roster peers while the list is non-empty. An empty list keeps the last non-empty one while the laptop that sent it is still nearby. Fleet members that announced they are seated are included too. Only while the socket is up |
| `want_omarchy_seat` | Routing seat intent |
| `holding_omarchy_seat` | Seat is wanted and the control socket is not up |
| `devices` | Open fleet members (`discovered: false`, `nearby` when a sealed sighting is fresh), nearby hosts available for enrollment (`discovered: true`), and all saved Apple devices (`kind: apple`, `nearby: false`). A saved member advertising that it is unjoined appears as discovered while that hint is fresh, unless fresh authenticated presence takes precedence. This does not erase saved membership. A saved entry does not imply a connection; `connected_peers` supplies that evidence. Omitted when empty |
| `fleet.phase`, `.id`, `.peer_address`, `.peer_name`, `.initiator`, `.expires_ms`, `.error`, `.code_attempt` | `fleet::enroll` |
| `fleet.code` | Inviter only, and only while the code is on screen |
| `fleet.enrolled` | Membership holds an open fleet |
| `fleet.waiting_for_keyring` | Saved membership cannot be read because the keyring is locked or unavailable. The daemon retries every five seconds; the panel shows “Waiting for keyring” and hides enrollment actions until a read succeeds. `enrolled: false` during this wait does not mean the saved fleet is absent |
| `devices[].fleet_enrolled` | Optional public discovery hint: `true` means the peer advertises an existing fleet, `false` means unjoined, and `null` means unknown. This unauthenticated hint selects setup wording and never authorizes membership or key replacement |
| `fleet.available` | `false` until routing advertisements are interpreted. A presence sighting is a discovered device for 30 s from its own timestamp |
| `fleet.routing` | `fleet::route`: phase, busy, admission, error, peer, retain |
| `fleet.roster` | Fresh matching peer report, otherwise `{}` |
| `fleet_outcome` | Last routing outcome whose Unix time is non-zero. Alongside and ordinary success stay different result strings |

Not emitted, on purpose: the old `*_total` counters, microphone PCM, fleet keys, and accessory keys.

Apple peer names are learned from `btName` introductions relayed by the paired AirPods.
The daemon introduces this host once per peer per control session, updates `devices`
when a name or rename arrives, and saves the names by Bluetooth address in
`${XDG_CONFIG_HOME:-~/.config}/fleetpods/devices.json`. Peers that do not announce a
name keep their saved label, or the address fallback if none has been learned.

When both fleet laptops are connected and idle, fresh desktop activity can request audio ownership over the existing control link. If the AirPods report loss of ownership without naming the new owner, a fresh authenticated self-report from the other seated laptop supplies that identity. Playing, pending, or expired reports cannot authorize this idle takeover. Sending the request does not mark ownership as transferred; the AirPods must confirm it.

Suspend retires the current control generation before the local socket closes. A close callback from that generation cannot turn an intentional suspend into lost ownership. After the wake grace, a previously ready idle connection can restore its control session if Bluetooth has already rejoined, without requesting audio ownership. Bluetooth being connected alone does not confirm an Omarchy seat; the plugin shows that control setup is still pending until the daemon reports readiness.

`native_handoff` is omitted until the audio server has reported or this host has yielded or is reconnecting an Apple device. `yielded`, `phase`, `audio_output`, and `audio_active` come from that handoff. The panel treats a missing object as no local handoff block.

During first-run setup, the first pairing-mode advertiser takes display focus while its pairing reports are fresh (three seconds after the last advertisement). Other candidates remain separate and cannot overwrite that display while pairing is active. When it expires, normal case reports can take focus; an expired pairing-only advertiser cannot hold the screen using old RSSI. Selecting a device in the Bluetooth panel overrides this focus. RSSI is optional received signal strength in dBm, not a distance estimate.

During unpaired setup, a recognized Pro 3 pairing-mode advertisement makes FleetPods release its own Bluetooth discovery session and filter. FleetPods also enables the adapter’s Pairable property for this setup window so BlueZ can save a bond; it does not enable discoverability or call Pair. A previously bonded host does not change this property in response to another nearby device entering pairing mode. The Bluetooth panel can then own discovery and pairing. FleetPods waits up to two minutes after the last pairing advertisement, or until five seconds after BlueZ confirms a saved local bond, before resuming scanning. Before that pause, unpaired discovery includes classic Bluetooth as well as LE; paired background discovery uses LE. Already-paired hosts keep listening so they can observe pairing mode and suppress reconnects. This does not stop other applications' scans or initiate pairing. While no application is scanning, live case and pairing-light updates are unavailable.

Once AirPods keys are saved, a verified pairing-mode advertisement from the saved Bluetooth address or an address resolved by those keys suppresses reconnects for three seconds after the last such report. It cancels pending opens and retries, but leaves an established connection alone. Lid, activity, Play, and connect requests cannot start a connection during that interval. A verified closed-lid report also ends the hold. A new host without AirPods keys can still finish its first connection and retrieve them. Pairing-mode detection currently covers the verified AirPods Pro 3 advertisement format; another nearby pair of the same model does not trigger this hold.

Before pairing-mode focus, an observed open case with both buds docked takes priority over closed, worn, or unknown candidates. If multiple cases qualify, setup guidance follows the strongest recent smoothed RSSI; another qualifying candidate needs an 8 dB advantage to replace the displayed one. Without fresh open-and-docked evidence, the plugin first asks the user to put both AirPods in the case and open the lid. Signal observations expire after two minutes and are never inferred from cached name updates. Pairing-mode focus overrides signal ranking.

Public setup presence (version 5) advertises an optional enrollment flag alongside the host name and address, without a fleet identifier or key. Versions 2, 3, and 4 remain readable. An enrolled discovered peer offers **Join fleet**; an unjoined or older peer offers **Add to fleet**. A join request asks the enrolled peer to provide the code and initiate the authenticated exchange, preserving that peer's fleet key.

When this computer already has a fleet, Join first asks “You are currently in a fleet. Leave it?” Yes confirms a local fleet reset before requesting the invitation. No offers “Invite <device name> to your fleet instead?” A second No closes the choice without changing membership. An incoming invitation from outside the current fleet asks the same question before code entry. AirPods pairing survives leaving a fleet, and a failed keyring deletion prevents joining. Conflicting grants are rejected before any keyring write. The finishing stages have no Cancel button; closing them only dismisses the dialog.
