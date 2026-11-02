# Multiple AirPods accessories: design and phased plan

Status: **design agreed 2026-11-01. Phase A implemented (`a3d7806`).
Phases B–D implemented on top, uncommitted: radio selection follows
the daemon at runtime, wear promotion with per-pair prefs, `select`
IPC/CLI verb, and a panel pair switcher. The simultaneous-control-
link question from Phase B was settled by the one-active-pair
decision: only the selected pair holds a control link, so the Linux
layer switches its single link instead of holding two.**
Branch: `multi-accessory` (from `master` `7ae23dd`).

## Problem

The daemon tracks exactly one AirPods accessory. While AirPods Pro 3
are selected, a second paired set (for example the sacrificial
AirPods Pro used in the Fleet Pairing spike) is invisible: its
in-case / in-ear / battery state never updates, because
`Daemon::hear_proximity` drops every advertisement that is not the
selected accessory or resolvable by the one stored IRK
(`src/daemon.rs`), `Membership` holds one `AccessoryKeys`
(`src/fleet/members.rs`), the keyring holds one
`magic-acc-irk` / `magic-acc-enc-key` item pair
(`src/linux/keyring.rs`), and status projects one flat accessory
(`src/panel/status.rs`).

## Decisions (user, 2026-11-01)

- **One active pair.** Both pairs stay tracked; only one owns
  playback/microphone at a time. The OS has one default sink and one
  mic source, so simultaneous audio is not a user-visible feature.
- **Wear/play promotes.** Putting the non-active pair in-ear or
  pressing play promotes it. It never steals while the active pair
  is actively playing.
- **Preferences are per pair.** Noise mode, ear behavior,
  conversation awareness, adaptive level, and one-bud mode are
  remembered per accessory, not shared.

## What the mapping found

Leaf modules are already single-device-correct and reusable per
accessory unchanged: `pods::reports::Accessory`,
`pods::link::Connection`, `pods::lifecycle::Lifecycle`,
`audio::seat::Seat`, `pods::nearby::Nearby`, and the fleet
roster/route/pod-state wire formats (they already carry an accessory
address). The single-accessory assumption lives in ownership:

- `Daemon` fields: one `Config` (with `accessory: Option<Address>`),
  one `Connection`, one `Accessory` report, one `Seat`, one
  `Lifecycle`, plus one-accessory scalars (pod peers/order, route
  transaction, worn/inserted, displaced, …).
- `step.rs` addressing: `Input::Accessory` / `ControlSocket` /
  `Transport` are keyed only by `Generation`; `Effect::OpenLink`
  carries no address; `PendingEffects.control_out` is stamped with
  the *current* selected accessory at harvest time.
- Three global stores: `Membership.accessory_keys`,
  `fleet::activity::Preference` (one record), `fleet::pod_state::
  Sharing` (one scope).
- Linux layer: one `Live` (one control fd, one held accessory),
  `bluez::select` returns `None` when more than one paired set
  exists and none is configured, one persisted accessory file.
- Status/panel/CLI: flat single-device JSON, commands carry no
  accessory address, panel has no device switcher.

## Target shape

Per-accessory `AccessoryUnit` (address, per-pair prefs, report,
connection, lifecycle, seat, routing, keys, pod peers/order) owned
by `Daemon { units, selected, active }`, with host-global audio /
radio / keyring kept once. This is an ownership refactor, not a
rewrite of link, lifecycle, seat, or report logic. One `active`
arbiter decides which unit may claim the default sink and mic.

## Phases

- **Phase A — tracking (this branch, first):** per-accessory keys
  (`Membership` key map, keyring `magic-acc-keys-v2` map item with
  legacy migration), `Daemon` keeps a report per non-selected known
  accessory and updates it from proximity advertisements (placement,
  battery, case, lid, model), report swap on selection change,
  status gains an `accessories[]` array while flat legacy fields
  keep projecting the selected accessory. Connection, lifecycle,
  seat, and Linux runtime stay single (the selected accessory).
- **Phase B — Linux follows selection:** the radio worker's
  configured accessory is mutable state; a new `Job::Select` (from
  `Effect::SelectAccessory`) drops the outgoing pair's control
  socket, forgets its bond snapshot, and syncs to the newly selected
  device, which `bluez::select` already prefers when configured.
  The selection is persisted to `~/.config/fleetpods/accessory`, so
  a restart keeps the active pair. Simultaneous control links are
  deliberately not held: with one active pair (see decisions), the
  single link moves.
- **Phase C — promotion:** `Daemon::activate_accessory` is the one
  switch path (used by pairing, promotion, and the `select`
  command): outgoing report+prefs are kept, incoming report+prefs
  become live, the platform disconnects the outgoing pair and
  follows the selection. A tracked pair that transitions to in-ear
  is promoted unless the selected pair is worn, or local playback
  is streaming through it (`maybe_promote`). Connection, lifecycle,
  and seat remain the selected pair's, which is exactly the
  one-active-pair model.
- **Phase D — surface:** per-pair prefs in `Config` (`PairPrefs` +
  `saved_prefs`; a never-configured pair starts from defaults, not
  from the outgoing pair's choices), `fleetpods select <address>`
  CLI / `select:<address>` socket verb, status `accessories[]`
  (Phase A), `Model.accessories` parsing with address validation,
  and a panel switcher listing known pairs with an Active marker
  and a Use action.

Each phase keeps the single-accessory test suite green before the
next starts. New acceptance tests (from the mapping): two paired
accessories keep independent telemetry; connect/claim are
per-accessory; failover to the second pair without preemption when
the first returns.

## Phase A notes

- Keys are learned per accessory from its AAP link
  (`Observation::Keys` while it is selected) and stored under its
  identity address. A pair must be selected and connected once
  before another host state (e.g. after daemon restart) can resolve
  its rotating advertisements; from then on both pairs are tracked
  simultaneously.
- Legacy keyring items `magic-acc-irk` / `magic-acc-enc-key` are
  migrated to the configured accessory on first load.
- Fleet interaction (roster, pod-state sharing, activity
  preference) remains scoped to the selected accessory in Phase A.

## Hardware notes (2026-11-01)

- Pairing a second pair from `bluetoothctl`/scripts can silently
  take the LE path and time out: BlueZ pairs the device object it
  discovered, and FleetPods discovery surfaces pairs over LE. Run a
  classic inquiry first (`bluetoothctl scan bredr`) so the device
  has a BR/EDR bearer, then pair. Symptoms otherwise: attempts hang
  ~20 s, `org.bluez.Error.InProgress` if a wedged attempt lingers
  (restart `bluetooth` to clear), and the device drops out of the
  available list between tries.
- A newly seen pair's PipeWire sink can start at 0-6% volume
  (WirePlumber per-device restore), which looks exactly like a
  routing failure. Check the *sink* volume, not just the stream.
- Pro 2 (A2698) microphone validated: the Pro 3 ELD constants
  (ASC `f8 e6 30 00`, 48 kHz coding, 480 samples/frame, 64 kHz
  delivery) decode its mic stream correctly - an 880 Hz tone burst
  recorded at exactly 880 Hz with clean playback. When
  `fleetpods.hires-microphone` exists for an address, the
  WirePlumber hook hides the duplicate `bluez_input.<address>`
  source; pairs without a FleetPods mic keep the stock input.
