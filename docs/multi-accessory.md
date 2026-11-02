# Multiple AirPods accessories: design and phased plan

Status: **design agreed 2026-11-01; Phase A implemented on this
branch (417 unit tests + 33 acceptance tests green). Phases B–D not
started.**
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
- **Phase B — Linux multi-runtime:** per-accessory `ControlLink` /
  runtime map + poll set, `bluez::select` multi-device handling,
  per-address persisted selection. Gate: hardware spike proving two
  concurrent AAP control sockets on one adapter.
- **Phase C — connection/seat per unit:** address-tagged
  `Input`/`Effect`, per-unit lifecycle/seat/activity/pod-state,
  active-audio arbiter, wear/play promotion with no-steal rule.
- **Phase D — surface:** per-pair prefs in `Config`, status schema
  completion, IPC/CLI optional accessory address, panel device
  switcher with per-device batteries/settings and active marker.

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
