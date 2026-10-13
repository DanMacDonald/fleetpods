# Fleet enrollment hardware validation

Tested on 2026-10-13 using omarchy-xps and lg-gram, with FleetPods running on both.

Each enrollment direction started with two independent fleets, each containing only its own laptop. To prepare that state, Gram was removed from XPS's membership, Gram's fleet was reset through the CLI, and its daemon was stopped before storing a newly generated self-only fleet record. No AirPods keys or Bluetooth bonds were removed.

The live tests used the same CLI commands sent by the plugin. Invitations, code exchanges, cancellation and completion travelled through the running daemons and Bluetooth transport; no simulated transport or clock was used. Pairing codes were transferred in memory and were not printed or saved. Fleet secrets were not inspected.

| Scenario | Result |
| --- | --- |
| XPS invites Gram; Gram declines | Both reach `cancelled`, retain their independent memberships, and do not initiate a reverse invitation |
| Gram invites XPS; XPS declines | Both reach `cancelled`, retain their independent memberships, and do not initiate a reverse invitation |
| XPS invitation expires without code entry | Both reach `expired` after the real invitation timeout; published codes clear and independent memberships remain |
| Gram accepts an XPS invitation while already enrolled | Explicitly leaving clears Gram's previous membership while preserving the incoming code-entry session |
| Gram submits a wrong code, then the correct code | Wrong code returns to code entry without adding membership; corrected code completes on both machines |
| Restart XPS, then Gram after that join | Both retain mutual membership; both still report saved Bluetooth bonds and AirPods keys ready |
| XPS requests to join Gram while already enrolled | Explicitly leaving XPS's fleet starts the join request; Gram displays the code and XPS receives it |
| XPS submits a wrong code, then the correct code | Wrong code returns to code entry without adding membership; corrected code completes on both machines |
| Restart XPS, then Gram after the reverse join | Both retain mutual membership; both still report saved Bluetooth bonds and AirPods keys ready |

The focused `FleetChoice` and `FleetPairing` QML suites also passed: 15 test cases, or 19 checks including setup and cleanup. These cover incoming-decline dismissal, the alternative offered for locally initiated joins, explicit acceptance, code editing/retry, countdown, and close/cancel behavior. The tested dialog and panel files matched both installed plugins.

The live commands exercised daemon behavior; desktop dialogs were tested offscreen rather than clicked on the running desktops. Expiry was exercised with XPS inviting Gram. This pass does not cover every membership topology, repeated radio loss, or locked-keyring recovery.

Final state: both daemons running, enrollment idle with no error, and XPS and Gram together in Gram's fleet. AirPods pairing remains intact.
