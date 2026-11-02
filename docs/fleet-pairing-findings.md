# Fleet Pairing findings (spike, 2026-11-01)

Status: **proven end-to-end on sacrificial hardware; not productized.**
Nothing in this document is implemented in the shipped daemon. It records
what the spike verified, what it ruled out, and what a real implementation
must still solve. No key material is recorded here; all key files from the
spike lived at mode `0600` under `/tmp/fleet-pairing-spike/` and the Mac
PacketLogger captures still need deleting.

Test hardware: older AirPods Pro `10:B5:88:96:4D:AE` (sacrificial, factory
reset before the clean runs), XPS (`A8:72:4D:0C:7C:EE`), LG Gram
(`lg-gram`, `D0:C6:37:2F:D0:9F`), an M1 MacBook Pro as protocol reference.
The daily AirPods Pro 3 `CC:22:FE:54:DF:C7` were never written to.

## Goal and proven claim

Pair a headset once on one fleet device; every other fleet device then
connects silently — no pairing mode, no per-device pairing. This is
Apple's MagicPairing model applied to a fleet-shared account.

Proven, including a fully clean run:

1. Factory-reset the pods; pair them on the XPS the normal way (Bluetooth
   settings UI); write the fleet account key to them (provisioning below).
2. Factory/host state on the Gram: no BlueZ device, no derived key, no
   ratchet state — only the fleet master file.
3. On the Gram, with the XPS left powered on and the pods never put in
   pairing mode, one root helper performs the whole exchange and the pods
   come up as a connected A2DP sink (`Paired: no`, `Bonded: no`,
   `Connected: yes`, `ServicesResolved: yes`,
   `bluez_output.10_B5_88_96_4D_AE.1` `RUNNING`) and play desktop audio
   (test tone and YouTube both heard). Reproduced in a user-driven run:
   pair via the XPS UI, walk to the Gram, run one command, press play.

## Protocol (as implemented in the spike)

Fleet masters are two 16-byte secrets (`MasterKey`, `MasterHint`). Every
fleet host derives the same per-accessory values:

- `accKey = AES-ECB(MasterKey, BdAddrBlob)`, `hint = AES-ECB(MasterHint, BdAddrBlob)`
- `BdAddrBlob` is the 16-byte blob built from the **accessory** Bluetooth
  address (see `Addr Blob` handling in the spike scripts), so the derived
  pair is identical on all fleet hosts.

Provisioning (once, over the AAP L2CAP channel PSM `0x1001` while paired
to the first host): an AACP write with opcode `0x31` (not `0x32`, which
the pods silently ignore) carrying `magicAccKey` (`0x0008`) and
`magicAccHint` (`0x0010`). Entries for `MasterCloudIRK` (`0x1000`) and
`MasterCloudAddress` (`0x2000`) from a real Mac first-pairing capture
were *not* needed for the exchange to work.

MagicPairing exchange (fixed L2CAP CID `0x0030`), each connection:

1. Optional MP Ping `F0 00 00` (host → pods).
2. Hint (type `0x01`): entries `0x0010` hint, `0x0020` nonce_host,
   `0x0100` ratchet (little-endian u32).
3. Ratcheting (type `0x02`): entries `0x0080` AES-SIV blob (54 B) and
   `0x0100` ratchet_airpod. Plaintext (38 B) is
   `rand_airpod | nonce_host | accessory_addr(6)`. The SIV key is
   `ECB(accKey,"bt_aessivauthent") | ECB(accKey,"bt_aessivencrypt")`.
4. Answer (type `0x03`): one `0x0080` entry (80 B) sealing
   `nonce_host | rand_host | rand_airpod | hint` (64 B) under the same
   SIV key. The once-rotated-key variant was tried and is **wrong**.
5. Status (type `0xFF`, 3 B): byte 2 is the status (table below).
6. Link key derivation: `LK = ECB(rand_host, rand_airpod) XOR
   ECB(rand_airpod, 0*16)` — byte-wise **XOR** (the paper's figure
   prints a `+`; XOR is what the pods accepted).

Ratchet semantics observed: the pods report `ratchet_airpod = 1` and the
Ratcheting SIV verifies against the **unrotated** provisioned `accKey`
every session; failed attempts do not advance anything. A driver should
still persist `(accKey, ratchet)` and tolerate verification at a few
rotations around the reported value.

Status codes (from the MagicPairing paper's table): `0` Success,
`1` Internal Error, `2` Key Not Found, `3` Invalid Parameters,
`4` Pairing Busy, `5` Version Unsupported, `6` Operation Timed Out,
`7` Failed Verification, `8` No Keys for Peer.

## The critical finding: the Answer deadline

Every "mysterious" failure in the spike was one bug. The pods expire the
MagicPairing session soon after sending Ratcheting. The kernel
(mp_session-style) path answers in ~2.1 s; a Mac in ~10 ms. Our first
raw helpers answered at ~3.1 s — a helper collected replies with a 2 s
idle wait measured in whole seconds, plus subprocess-based crypto — and
**every** such attempt was rejected with Status `3`, whatever else we
changed. Returning the Ratcheting to the driver immediately (300 ms
millisecond-resolution idle) made the Answer succeed in both transports
(raw-owned HCI and injection on a kernel-owned ACL).

I wasted most of the debugging time on plausible-but-wrong theories.
Each of the following was implemented, tested, and **ruled out** as the
cause of Status `3` (framing of the transmitted Answer was additionally
verified field-by-field from a btmon capture: SIV verifies, nonce/rand/
hint all correct):

- AES-SIV/CMAC implementation error (validated against RFC 4493
  vectors incl. the 64-byte branch, and round-trips).
- ACL authentication/encryption being required (raw run with derived-key
  authentication and encryption enabled still got `3`; Apple exchanges
  run before authentication).
- PB flag (`0` vs `2`), L2CAP FCS, ACL packet types (Create Connection
  parameters are byte-identical to the kernel's `0xCC18`, R2, clock 0).
- L2CAP Information Request/Response contents: sequential vs parallel
  requests, feature masks (`0x80`, `0x1FF`, kernel-exact `0x2B8`),
  fixed-channel masks (with/without the CID `0x30` bit, kernel `0x86`).
- Answering the pods' Information Requests (necessary for the Hint to be
  answered at all on some paths, but not sufficient).
- Accepting/configuring the pods' L2CAP Connection Requests (SDP) and
  the MP Ping/MTU trick; retrying a full Hint→Answer cycle in-session.
- Sealing the Answer with the once-rotated key.
- Pods-side key history (a factory-reset pristine run failed identically
  before the timing fix, and succeeded after it).

Transport facts that are true independent of the deadline:

- The pods only answer the MP Hint once the host behaves like a real
  L2CAP stack: bidirectional Information exchange (kernel-style feature
  mask `0x2B8`, fixed mask byte `0x86`). Ignoring the pods' Information
  Requests (our first raw helper) yields total silence.
- Kernel fixed-CID sockets refuse unbonded connects (`EACCES`): the
  kernel initiates pairing, and the pods refuse outside pairing mode.
  `SOCK_RAW` L2CAP does **not** bypass this — binding a fixed CID
  reclassifies the channel as `L2CAP_CHAN_FIXED`. `BT_DEFER_SETUP`
  does not help either.
- An `HCI_CHANNEL_USER` raw helper (device taken down, kernel detached)
  works and is what the spike used; an `HCI_CHANNEL_RAW` injector on a
  kernel-created ACL (held open via an SDP connection) also works.
  Either way the helper needs `CAP_NET_RAW`-class privilege.

## BlueZ integration facts

- `Load Link Keys` (MGMT `0x0012`) **replaces** the kernel key set; it
  never merges. Sending a partial set wipes the stored keys (we did
  this twice; `btmgmt keys` and `bluetoothctl`'s MGMT `keys` do it
  too). Restoring requires `systemctl restart bluetooth`. Any product
  loader must always send the union: every stored link key from
  `/var/lib/bluetooth/<adapter>/*/info` plus the fresh derived key.
  Derived keys are accepted by the pods for later authentication.
- Derived keys persist only until bluetoothd reloads its stored set, so
  loading must happen per connection, before profile connection.
- A BlueZ device object must exist before `bluetoothctl connect` will
  do anything. After `bluetoothctl remove` plus adapter restarts there
  is no object, and the pods advertise with resolvable private
  addresses that BlueZ cannot map without the IRK. Creating any ACL
  (e.g. an SDP L2CAP connection) makes bluetoothd register the device.
  But connecting profiles on an already-open *unauthenticated* ACL
  fails with `br-connection-refused`; disconnect fully, then reconnect
  so BlueZ authenticates with the loaded derived key first — profiles
  then connect and the A2DP sink appears.
- Bond persistence with these AirPods under bluetoothctl/UI pairing was
  flaky in this environment: `Paired: yes` repeatedly reverted to
  `Paired: no` (and an empty `[LinkKey]` section in the BlueZ `info`
  file) after our adapter power-cycles and `HCIDEVDOWN` runs. Do not
  power-cycle adapters mid-flow; treat BlueZ bond state as untrusted
  and read it fresh before each step.
- Audio verification trap: PipeWire's stream-restore had our `paplay`
  test stream at 0% volume while the sink showed `RUNNING`. Verify
  with a real app stream (or explicit `--volume`) before concluding
  audio does or doesn't work.
- Other hosts will steal the pods: an iPhone with Bluetooth on
  repeatedly took them mid-test (disconnect chime on BT toggle was the
  tell). Isolate other hosts during debugging.

## Working recipes (spike scripts in `/tmp/fleet-pairing-spike/`)

XPS (pair once): pair normally in the Bluetooth UI → run
`write_probe2.py` (AAP provisioning, user-level) while connected. An
mp_session kernel exchange should report Status success.

Gram (silent): with `derived_lk.bin`/ratchet state absent and fleet
masters synced, run `fleet-gram.sh` under sudo. It stops bluetoothd,
runs `full_exchange.py` over `hci_mp5` (HCI_CHANNEL_USER, fast replies),
restarts bluetoothd, loads the derived key (`load_derived.py`), and
connects. Then register/connect profiles as described above if the
device object is missing.

The full exchange prints only structural data (message types, entry
lengths, status bytes, counters) to run logs under
`/tmp/fleet-pairing-spike/runs/`; key values are never logged.

## What a product implementation still needs

- A privileged helper: the daemon cannot do raw HCI or `Load Link
  Keys` unprivileged. The design constraint is no *runtime* root;
  see "Privilege design: no runtime root" below for the chosen
  capability-scoped helper sketch. Driving the kernel socket after
  pre-loading a derived key is not enough — the first derivation on a
  never-paired host cannot use the kernel socket (pairing refusal
  above), so some raw path is unavoidable for v1.
- Fleet distribution of `MasterKey`/`MasterHint` and ratchet state,
  reusing the existing fleet enrollment/keyring machinery rather than
  files in `/tmp`.
- Per-connection derived-key lifecycle: derive → union-load → BlueZ
  device registration → authenticated profile connect → set sink,
  folded into the connection lifecycle instead of shell scripts.
- UX for the one pairing: pair + provision must be one gesture in the
  panel; the Gram side must trigger from playback intent (current
  FleetPods seat/activity machinery) with no terminal.
- Decide how this interacts with Apple-account pairing: provisioning a
  fleet account coexisted with normal pairing in the spike, but only
  sacrificial pods were used. Validate against a user's real pods and
  iCloud behavior before shipping.
- Expect marketplace scrutiny: this is exactly the kind of privileged
  Bluetooth key handling behind the submission's
  `security-review-required` / `needs-fixes` labels.

## Privilege design: no runtime root

There is no root-free implementation on stock Linux/BlueZ, and that is
a kernel boundary, not a missing trick. Two operations are gated:

1. Sending MagicPairing frames to an **unbonded** device. The kernel
   L2CAP socket refuses (`EACCES`, see transport facts above) because it
   insists on pairing first, and the pods refuse pairing outside
   pairing mode. Raw HCI needs `CAP_NET_RAW`/`CAP_NET_ADMIN`. The
   security level cannot be lowered: `BT_SECURITY_SDP` is rejected for
   userspace (only in-kernel SDP/3DSP channels may downgrade), so there
   is no supported flag that skips this.
2. Loading a link key (MGMT `Load Link Keys`, `0x0012`). A loaded link
   key *is* device impersonation, so the kernel rightly keeps it
   privileged.

This is the same reason `bluetoothd` itself runs as root: every
apparently rootless Bluetooth feature is a user process asking a
privileged daemon to act. The achievable product goal is therefore:
**privilege exists once, at install time, in the smallest possible
box. The user never sees a sudo prompt at runtime.**

### Sketch: `fleetpods-paird`, a capability-scoped system helper

One new system component, installed by the package (which already has
install-time sudo, like every system package):

- **Process shape**: a socket-activated systemd system service, e.g.
  `fleetpods-paird.service`, running as a dedicated system user with
  `CapabilityBoundingSet=CAP_NET_RAW CAP_NET_ADMIN`,
  `AmbientCapabilities` matching, `NoNewPrivileges=yes`,
  `ProtectSystem=strict`, no home, no network beyond the local
  Bluetooth AF families (`RestrictAddressFamilies=AF_BLUETOOTH AF_UNIX
  AF_SYSTEM`). No shell, no root login, no root process: root-class
  *operations* confined to two capability bits in one ~small binary.
- **Interface**: D-Bus on the **system** bus, one object, one verb,
  peer credentials checked (only the FleetPods user / an explicit
  polkit rule may call):
  - `DeriveAndLoad(adapter: s, accessory: s, acc_key: ay, hint: ay,
    ratchet: u)` → `(result: s, new_ratchet: u)`, where `result` is
    `loaded`, `key-not-found`, `not-provisioned`, `timeout`, or
    `busy`. The user daemon derives `acc_key`/`hint` from the fleet
    masters (existing keyring) and passes them per call; the helper
    performs the whole privileged sequence internally and returns
    only the enum and ratchet. **The derived link key never leaves
    the helper:** it computes it, union-loads it with the stored
    BlueZ key set, and discards it.
- **What it does per call** (all proven in the spike):
  1. Ensure an ACL exists without firing BlueZ profiles
     (attach-style injection on a kernel-owned ACL, held open via an
     SDP connection, rather than the spike's `HCI_CHANNEL_USER`
     device-takedown, so the helper never fights bluetoothd for the
     controller).
  2. Fast MagicPairing exchange on fixed CID `0x0030` — kernel-style
     Information exchange, Hint → Ratcheting, Answer within the
     session deadline (300 ms reply discipline, see the critical
     finding above).
  3. Derive the link key; union-load: all stored keys from
     `/var/lib/bluetooth/<adapter>/*/info` **plus** the derived key
     (never a partial set — `Load Link Keys` replaces).
  4. Return `loaded`. The user daemon then registers/connects the
     BlueZ device and the authenticated profile connection proceeds
     as in the normal FleetPods lifecycle.
- **Explicitly not its job**: provisioning (the AACP `0x31` write is
  already user-level over the AAP L2CAP channel on the paired host),
  fleet master distribution (existing enrollment/keyring machinery),
  seat arbitration, audio policy, UI. Masters live in the existing
  fleet keyring; the helper receives `accKey`/`hint` per call over
  the authenticated channel and retains nothing on disk — ratchet
  state is passed in and returned per call and owned by the user
  daemon's existing state store.
- **Alternatives considered**: a BlueZ plugin running inside
  bluetoothd (no extra service, but C code in bluetoothd's process —
  larger review blast radius, and plugin install still needs root
  once); a setuid helper (works, but a worse look for the marketplace
  security review than file/systemd capabilities); driving only the
  kernel socket after pre-loading a key (impossible for the *first*
  derivation on a never-paired host, per the pairing refusal).
- **Security-review posture**: this structure is the strongest
  available answer to the `security-review-required` label — keys
  exist only in the helper's memory, the helper's syscall surface is
  one verb, and everything user-facing stays in the existing
  unprivileged daemon.

### Daemon integration (single-accessory phase)

The daemon today tracks exactly one selected accessory
(`Config.accessory`, `select_accessory`, singular fleet
`AccessoryKeys`), so phase 1 integrates Fleet Pairing for that one
accessory only:

1. **Provision step** after first normal pairing of the selected
  accessory: user-level AACP `0x31` write of `magicAccKey`/
  `magicAccHint` derived from the fleet masters (needs a `fleetpods
  pair-provision` affordance in the panel/CLI, one gesture after
  pairing).
2. **Derive step** in the connection lifecycle: when the selected
  accessory is in range but has no BlueZ bond/derived key on this
  host, call `DeriveAndLoad` before the usual connect attempt,
  triggered by the existing seat/preferred-connector machinery and
  by playback intent (the same triggers that already decide when
  FleetPods tries to claim the pods).
3. **Connect step** unchanged: BlueZ device registration, fully
  disconnected start, authenticated profile connect, A2DP sink.

Phase 2 (multi-accessory: per-device reports, connections, key sets,
panel selection) is a separate refactor of the single-accessory
assumptions and should not be conflated with Fleet Pairing itself.

## Cleanup owed

Delete the spike secrets and captures (`/tmp/fleet-pairing-spike/`
secret files and `captures/`, the Mac `.pklg` files and Bluetooth
logging profile), restart the FleetPods user services that were stopped
for the spike, and turn the XPS adapter back on if a test left it off.
