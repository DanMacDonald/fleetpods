# Gram first-run validation

Tested on 2026-10-13 with lg-gram and AirPods Pro 3. XPS remained an enrolled peer for the join test.

With the user's confirmation, Gram's AirPods Bluetooth pairing, saved AirPods keys, fleet membership, and remembered device names were cleared. Its old plugin, binaries, service, and microphone hooks were removed or moved to backups. Gram was also removed from XPS's saved membership. Other Bluetooth devices were not forgotten.

The current source was copied to a temporary directory on Gram and built with Cargo. Cargo was installed in `~/.cargo/bin` but absent from the SSH PATH; supplying that directory resolved the environment issue. The documented `./setup` command then installed and enabled the service, plugin, and microphone hooks successfully. The installer restarted WirePlumber to load the newly installed hooks.

| Check | Result |
| --- | --- |
| Clean welcome state | Plugin showed “Pair AirPods to get started,” empty seats, closed-case fallback artwork, and no battery indicators. Daemon reported unpaired, no AirPods keys, and no fleet membership |
| Pair through Gram's Bluetooth panel | User confirmed pairing and connecting. Daemon confirmed a saved Bluetooth bond, a live connection, and learned AirPods keys |
| Restart daemon after pairing | Saved bond and AirPods keys loaded successfully; Gram remained unjoined |
| Close case, then reopen | Observed closed lid and Bluetooth disconnection while pairing and keys remained intact. Reopening reconnected without pairing again |
| Join XPS's fleet through Gram's plugin | User selected Join fleet, initially entered a wrong code, then corrected it. Both daemons reached success and confirmed mutual membership |
| Check saved item names | `fleet-v1`, `magic-acc-irk`, and `magic-acc-enc-key` were present after joining. Only item attributes were inspected; secrets were not read or printed |
| Apple-seat Reconnect initiated from Gram | Completed in 12.7 seconds. Gram disconnected and returned to control stage `ready`; saved pairing and AirPods keys stayed intact throughout. No reconnect error remained |
| Final plugin state | Gram occupied the Omarchy seat and iPhone occupied the Apple seat, with XPS in the tray. The plugin did not return to pairing instructions |

Reconnect briefly reported `retrying` between handshake and notification setup before recovering automatically. The captured logs did not establish its precise cause. This is a successful recovery, not evidence of a retry-free handoff; repeated reconnect reliability remains part of the hardware regression pass.

The join completion check briefly preceded the published roster settling. A subsequent check confirmed both memberships without intervention. The earlier public unjoined presence may outlive completion momentarily; this pass did not measure that interval precisely.

Bluetooth pairing and fleet code entry were performed by the user through the desktop panels. The final Reconnect used the same `fleetpods reconnect-peer` command as the Apple-seat control. The daemon remained running without an automatic crash restart.
