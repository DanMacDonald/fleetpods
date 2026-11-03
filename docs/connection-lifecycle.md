# Connection lifecycle

`Lifecycle::advance` drives one sequence. A single forward pass checks its launch
points and transition gates; diagnostics read the first unmet gate. Pending tasks
retain their identity and budget across phases. Resource loss goes through the
same gates, without a separate phase-repair path.

```mermaid
flowchart LR
    A[Availability] -->|admission| B[Linking: OpenLink]
    B -->|Bluetooth up| C[PreparingPaths: A2DP and codec]
    C -->|profile succeeds or output already exists| D[CompletingProfiles: arm HFP]
    D -->|output exists| E[Routing]
    E -->|routed, output running and transport active| F[Streaming]
```

A fast observation can cross several phases in one step. Background control
settles at ControlReady without claiming audio. Link retry, failure and
interruption have explicit states. Output disappearance returns to acquisition;
recovery returns to Routing. These transitions add no sleeps.

## Who owns what

| Owner | Lifetime and work |
| --- | --- |
| `daemon::session` | Admission cancellation, claim/release and rejoin operations; private background intent, deferred claims, ownership timing, player resumes and recovery bookkeeping |
| `Lifecycle` | Connection phases, once-per-generation OpenLink request, A2DP/codec launch conditions and success observations, optional HFP, resource cancellation |
| `retry::Task` | Control setup, A2DP, codec and output-verification attempt identity, retry timing and budget, callback/confirmation deadlines, stale-reply rejection; fleet advertisement registration budgets |
| `pods::link` / `pods::setup` | Control socket setup, AACP exchange, bounded link retries and interruption; Connection contains the sole Apple reconnect sequence |
| `audio::route::Router` | Saved stream identities, speaker muting, stream moves/restoration and output recovery |
| Linux Bluetooth/PipeWire adapters | Actual handles and asynchronous calls; report observations and tagged completions |

Admission/preference/pairing rules supply permission to open. The lifecycle
issues the request in Linking; there is no separate daemon launch flag. BlueZ
service discovery runs with the physical connection and reports its result.
Fleet admission reserves a connection slot; it does not imply playback intent.
A background join must not pause the current owner or create a speculative resume
hold. The authenticated roster's playback-intent flag is the single early-yield
trigger, preserving advance release for actual takeovers before a control claim
can arrive.

Permission to open is not permission to take audio: `Facts.intent` is
`JoinInBackground` until the router holds a playback claim (`TakeAudio`), and
only `TakeAudio` pursues profiles, routing and streaming. Ownership claims are
written in one place (`Session::emit_ownership_claim`); desktop activity may
claim an idle fleet owner immediately, but an outside device that only just
reported not playing is still joining and cannot be claimed for 10 s.

Desktop activity uses one event path. The shell timestamps an observed idle-to-active
transition (or a panel-open action) before its screensaver check, and sends
`desktop-activity:UNIX_MS`. The daemon converts it to monotonic time, rejects stale,
future or repeated observations, and keeps the original timestamp when deciding
whether input followed a yield. Remaining active generates no input heartbeat.
Repeated unsolicited player starts do not become takeover intent; fresh input
expires after 15 seconds. Bare CLI `activity` remains an explicit request made now.
The timestamped verb has a distinct prefix and requires a newline; any partial
read waits for the complete message instead of turning it into fresh activity.

Startup player snapshots supply observations, never new takeover intent. For a
worn headset, the existing owned-playback routing path can restore playback only
when the pods directly confirm local ownership and playback has not been yielded.
The same owner handles newly observed streams. Explicit Play and insertion retain
their existing claim paths; snapshots cannot reclaim a released or remote seat.

Fleet advertisement intent belongs to the Linux radio worker. Pairing temporarily
blocks registration without clearing that intent, including a first request
received during pairing. One reconciler registers or retires the BlueZ object.
Its `retry::Task` allows five registration attempts, with 5/10/20/40-second waits;
the worker schedules those deadlines even without scan or socket traffic. Payload
updates and pairing holds preserve a failed attempt's budget. Explicit release
followed by new intent, removal of a successful registration, or a bluetoothd
restart starts a new registration lifetime. D-Bus call timeouts report failure to
the task; tagged late replies cannot complete a newer attempt.

Removal is a separate operation under that same radio owner, using the shared
retry task with the same five-attempt backoff. Failure, timeout or a failed submit
leaves cleanup pending and blocks registration, even if intent or pairing changes.
Only successful unregister, DoesNotExist, an authoritative Release, or BlueZ loss
confirms that the slot is gone. Exhausted cleanup remains blocked until an
authoritative removal or BlueZ restart; payload updates cannot replenish its budget.
A timed-out registration also needs confirmed cleanup before retrying. Removing
that uncertain slot preserves the failed registration's budget. Cleanup replies
are tagged so an old success cannot remove a replacement registration.

Max 2 wear is an observation, not another connection lifecycle. `pods::air` decodes
the model-specific aggregate bits; headset control reports preserve both cup
readings. `Accessory` owns that observation and source precedence: after a control
wear report arrives, advertisements cannot overwrite it until the link ends or the
selected accessory changes. Battery-only reports do not invent wear. `Accessory`
answers wear/availability queries. Existing saved-address and IRK checks
gate updates to a saved accessory. Tracked wear feeds the existing selection gates;
selected wear feeds the existing insertion and owned-player pause/resume debounce.
Lifting either headset cup settles after 100 ms; earbud reports and full re-wear
retain 400 ms. Both cups feed the existing configurable one/two-part removal
policy on Max 2; one cup still worn does not permit the off-head speaker fallback.
Older and unidentified headsets retain the primary-reading policy until their
two-cup layout is verified. Accessory owns that model decision and keeps both raw
readings; setup does not acquire a separate model lifetime.
Both use the same pending deadline and pause/resume owner. Wear events apply one
action from `Audio::ears`; timer ticks settle outstanding debounce deadlines.
A subsequent manual Play while the Max is confirmed off-head uses the existing
speaker fallback, just like Play while buds are docked. `Accessory::playback_blocked`
owns that decision for playback, deferred claims, peer intent and output repair;
removal alone still pauses without rerouting. See the [wear transition audit](wear-transitions.md)
for equivalent behavior, deliberate case-only rules and remaining display gaps.
A new accessory's first report is not an insertion, and removal retires pending
insertion intent. No timer, retry loop or connection launch lives in the BLE
handler. Background admission uses the same availability query as preferred-host
reconnection and the published waiting/idle status, preserving fleet preference
and explicit-disconnect rules. `Accessory::connection_available` owns that query;
`playback_available` remains stricter because an open case alone must not mute
speaker playback.

For Max 2, proximity alone no longer authorizes automatic playback preparation:
a worn report must be fresh (ten seconds for availability). Missing reports become
unknown rather than generating removal. Advertisement wear expires even while the
control link is up; control wear stands until link loss, when it is cleared and fresh
advertisements may take over. Either BLE sensor counts as worn; one-cup removal is
not inferred from this aggregate report. Older Max advertisement behavior is unchanged until
its wear layout is verified.

BlueZ inventory reconciliation owns connection evidence for non-selected paired
accessories. Removal, loss of pairing or replacement of the BlueZ instance retires
that evidence; saved identity and keys remain in the daemon. Selecting an accessory
hands observations to its selected connection and retires the inventory dedup entry.

Microphone support is selected by SKU in `audio::mic::format_for_sku`; Max 2
uses the existing format, source and capture lifetime. `pace_microphone` offers
one source for the selected ready control link. The PipeWire adapter observes
links from that source and reports demand only while it is streaming and has
a client, so `set_microphone` ends capture even if the playback graph is still
running. Stream binding and link observations share one PipeWire core connection,
and both are retired with that source lifetime. Demand starts capture;
mute, client departure or control loss stops it through the existing owner.
No Max-specific capture worker or retry policy is introduced.

## Where to change a connection's lifetime

`Daemon::step` applies an input, then calls `advance_session` once. That is the
single place to read or change advancement order. Clock inputs first expire
observations and advance deferred claims, admission, link and reconnect timers.
Every input then advances rejoin/preference/ownership decisions, peer and ear
actions, fleet effects, final routing/setup work, and the final announcement.
The tick-only distinction is passed to this call; it is not another stored flag.

`daemon::session` is the coordination point. Command handlers and input dispatch
call operations there instead of resetting setup, routing intent and player
queues separately. `apply_claim` starts a claim; `release_playback` retires its
setup requests and deferred player work together. Release behavior is explicit:

| Operation | Playback after release |
| --- | --- |
| Handoff or disconnect | Quiet the pods output and pause players |
| Apple reconnect | Quiet and pause, then begin the reconnect sequence |
| Return to speakers | Keep players running on speakers |
| Release preference | Release the route without pausing players |
| Await availability | Return to speakers and retain intent for a deferred claim |

`finish_session` coordinates recovery, plans the route once, advances setup using
the final route facts, then drains work and resumes held players only after
recovery finishes. Handlers update intent and observations; they do not call
`replan` or replace queued routing work. It owns the effect ordering:
quieting precedes pauses and control writes. The input dispatcher does not
manage recovery fields or decide where setup effects must be inserted.

This is an implementation module on `Daemon`, with private `Session` fields,
not a second connection state machine. Fleet admission and physical availability
still supply policy; link setup and the router retain their own transactions.
Changing the coordination between them belongs here; changing an A2DP launch or
join belongs in `Lifecycle::advance`.

Admission queries (`connection_hold`, `wake_can_open`) are read-only.
`enforce_connection_admission` applies physical holds; preference loss uses the
same `cancel_connection_attempt` operation. Stowed pods may require a disconnect;
pairing and preference holds disconnect only if an open was actually requested.
An established control link and an explicit user disconnect keep their existing
rules. Playback release remains a separate operation: waiting for availability
must not pause media on the speakers.

Apple reconnect has one phase owner, `Reconnect`, contained by `Connection`.
Connection applies rejoin/failure transitions and cancels the sequence when the
link is interrupted. Session supplies whether players are quiet and peers have
left, then handles peer requests and playback feedback. It does not maintain a
second releasing/rejoining flag. Automatic recovery from an unexpected eviction
is a different operation: its saved players and pending rejoin live in Session.
After a failed Apple reconnect, Session can request one delayed background open.
Connection waits three seconds and owns a single three-attempt budget. There is
no session retry counter or timer around that operation. Admission still uses
session availability and preferred-host rules; stowage, radio loss, preference
loss and explicit disconnect retire the pending work. This deliberately replaces
counting reopen requests (which could do nothing during link backoff) with a
bound on actual connection attempts. Busy deferrals repeat the same budgeted
attempt, as they do for ordinary connects.

Control protocol progress and its deadline belong to `Setup`, contained by
`Connection`. The packet parser advances handshake → features → notifications;
callers no longer mirror those transitions or reset the parser separately.
`Connection` admits attempts and retries, and commits readiness after the
packet's reports are applied. Duplicate socket notifications cannot restart the
handshake. Reports use one `Observation` vocabulary; battery/ear correlation
still belongs to the parser. Current-generation reports remain usable outside
an active attempt, but cannot advance its handshake or issue setup writes.

## Forks and joins

PreparingPaths starts A2DP when the profile is missing and the authorized
Bluetooth link is up, so
control and audio never page the pods simultaneously. Codec setup can start
alongside it if control is ready and the card/profile exists. It advances once
per daemon step, including after routing starts. An existing hands-free sink
still needs an initial A2DP request; once acquisition has started, available
output satisfies the join without repeating a failed request.

CompletingProfiles arms HFP. Its resource state waits for both the successful
A2DP reply and service discovery, then an idle profile worker. It can start or
finish after the lifecycle reaches Routing or Streaming. Unsupported HFP is
skipped; failure does not block an available output. Codec selection likewise
does not introduce a playback barrier. The main joins are **output exists**,
then **routed, output running and transport active**—not "every setup task completed".

The router still owns acquisition muting and recovery. New codec requests wait
while recovery owns the card. During recovery the router waits
for restart, relink, settling and Start completion before resuming held players.
Control-ready ownership requests remain part of AACP integration; a sent claim
is not proof of ownership or streaming.

## Resource states

Control setup, A2DP, codec selection and output verification each own a
`retry::Task`. Its state enum is `Idle → Running → Succeeded`, with `Confirming`,
`Backoff`, `Deferred` (busy without spending another attempt) and `Exhausted`
branches. The former separate A2DP and codec progress enums have been removed;
status names are projections of this state, not additional bookkeeping.

A2DP's pending request survives control readiness. Busy/failure/timeout results
retain the existing two-second retry delay and thirty-second failure window,
measured from the first failed reply and checked on later failures. Its existing
BlueZ call timeout remains responsible for reporting an unanswered call. Early
exhaustion can start a fresh window once control becomes ready.

HFP uses `Idle → Waiting → Connecting → Finished`, or `Unsupported`.
Waiting carries the A2DP request it depends on; it is not a main-phase barrier.

Codec selection allows three attempts with one second between failed attempts.
Each request has five seconds for its callback, then a successful callback has
five seconds for graph confirmation. Only seeing the requested profile on the
card proves selection. A timeout retires that request even if recovery or a
missing card currently prevents another launch. Neither condition consumes
additional attempts. An existing usable output can keep streaming after codec
exhaustion; codec selection is not a new playback barrier.

A changed card or requested profile, a new claim, or audio-server loss starts a
new codec operation. A temporary missing graph/card does not replenish the same
target's retry budget. A later graph confirming the target is authoritative even
after exhaustion. If an already-selected profile changes externally, it requires
a fresh selection.

Output recovery uses `Idle → Parking → Resetting → Relinking → Settling → Starting
→ Idle`. Each variant carries only the request and deadlines valid in that
phase. Losing the settled route returns to Relinking. The 250 ms settle, 1.5 s
reset timeout and 5 s overall timeout are unchanged. Release or audio-server
loss retires the entire recovery; late reset and Start replies are ignored.

All three asynchronous result paths are scoped by request identity. New claims,
yield, link generations and accessory changes retire previous requests. Audio
server loss retires codec work while Bluetooth can remain connected. Physical
work already sent can finish; its stale completion cannot affect a replacement.
The existing Bluetooth-call timeout and audio-server disconnect reporting remain
the platform mechanisms for detecting lost work.

## Shared retry mechanics and failure policies

These are the current policies, not interchangeable forms of reconnect:

| Work | Failure handling |
| --- | --- |
| Control link and handshake | One shared task through control readiness; five attempts with 1/2/4/8-second backoff; Busy defers five seconds without spending another attempt; explicit setup stages keep their deadlines |
| Background open after failed Apple reconnect | Initial three-second delay, then three budgeted attempts (Busy deferrals exempt) using the same connection task and backoff; no outer retry loop |
| A2DP profile | Two-second retry delay within a thirty-second failure window; early exhaustion can restart after control readiness |
| HFP completion | Optional one-shot; failure does not hold playback |
| Codec selection | Three attempts, one-second retry delay, five-second callback deadline and five-second graph-confirmation deadline |
| Output recovery | Router owns park/reset/relink/settle/Start; shared task allows three attempts total (a direct takeover uses the first), verifying running output and active transport 2.5 seconds after each successful Start. Reset/Start failure or router timeout ends recovery |
| Output health repair | Three-second stall grace and thirty-second cooldown; admitted repairs use the output recovery task and its attempt budget |

`retry::Task` is used by control setup, A2DP, codec selection and output recovery
verification. `Policy` selects fixed or staged delay, an attempt-count or
failure-window limit, and optional request/confirmation timeouts. `Requests` supplies identities shared by the lifecycle tasks; the
connection and session have their own sequences for control and output recovery.
No sequence rewinds on task reset. Tasks emit no platform effects: `launch` returns an ID at
the lifecycle's existing launch point, `reply` accepts only the live attempt,
and `observed_success` records an authoritative resource observation. `tick`
can retire an expired attempt without launching a replacement. `try_launch`
admits an attempt through a resource operation: a router that cannot yet park
the stream rejects admission without spending the retry budget. Owners classify
failures as retry, defer without another charge, or stop. Each deferred launch
gets a fresh request ID even though it reuses the same attempt budget.

The task owner must still define success: a callback acknowledgement, observed
profile, ready control link and active audio transport are different facts.
Recovery must finish before a retry is admitted, and released/superseded claims
must not be revived by a timer. Pure elapsed-time accounting cannot decide those
conditions.

Output verification lives in the session alongside the playback claim. Its
existing owner encapsulates Start acknowledgement, verification, retry admission
and cancellation; session orchestration does not inspect the shared task state.
One recovery-failure operation retires verification, held players, resume echoes
and the post-rebuild pause window for reset failure, Start failure or router
timeout. Player observations and rebuilds use the same deduplicating resume queue.
Connection intent is decided once for both failure callbacks and ticks; terminal
background intent is cleared once during session advancement. Its
Running state covers the router's entire rebuild. A successful Start reply
begins confirmation; a running output and active Bluetooth transport at the
2.5-second check complete it. A brief transport blip before that check is insufficient. Players
resume at Start completion, before confirmation, so verification cannot hold up the
very playback it needs to observe. Losing the grant, releasing or replacing the
claim, or losing the audio server retires verification. If the player is paused
at the confirmation deadline, that check is discarded without a deferred retry.
A new ownership recovery can start a fresh budget after success or exhaustion.

Connection retains explicit connecting/handshake/features/notifications stages
and their 25/2/25/22-second deadlines. Those stages belong to one task attempt;
only control readiness completes it. There are no separate link attempt counters
or backoff deadlines: status projects them from the task. Stale completions
cannot revive setup during backoff. An intentional Apple socket release has an
explicit state, timed by the reconnect cadence, rather than an infinite setup
deadline. A continuing playback claim can still open a fresh five-attempt
operation after the existing 16-second terminal-failure cooldown; background
intent is cleared on exhaustion and does not renew itself.

Takeover, steady-playback watchdog and manual output repair share
`OutputRecovery`: one retry budget, request identity and cancellation owner,
with the router executing park/reset/relink/settle/Start for every rebuild.
There is no separate suspend/resume worker or completion path. A new claim
retires any previous recovery, so its late reset/Start replies cannot advance
or resume players for the new claim. Already-issued platform operations are
not undone; the profile commands run in order on the Pulse connection.

Health detects a stall after three seconds and requests recovery, committing
its thirty-second cooldown only when the router admits a rebuild. It keeps
no separate retry counter. The watchdog cannot renew an exhausted recovery
budget. An explicit manual reset can supersede verification only when the
router is no longer recovering. Each rebuild pauses and later resumes the
players through the existing session lifetime; a successful Start begins the
2.5-second playback check rather than proving that sound reached the listener.

The direct takeover path is reserved for outputs without a known node failure.
Output observations retain an unlinked/error report (`-3`) across subsequent
suspension until that node runs, disappears, or is replaced (sink name and Pulse
index identify it). A grant for that failed output immediately parks streams
and rebuilds it, without spending the 2.5-second direct-path verification wait.
Ordinary suspension alone still permits direct startup. Lifecycle Streaming
and takeover verification both require a running output and active transport;
neither a successful Start command nor transport activity alone proves readiness.
Optional HFP remains one-shot. There are no unused generic recovery strategies
or a separate task scheduler; connection launch/join decisions stay in
`Lifecycle::advance`, and playback recovery stays in session advancement.

## Verification

Tests cover admission, background connections, bounded retries, late completion
across phases, discovery/A2DP ordering, optional HFP failure, cancellation,
replacement generations and codec targets, audio-server loss, and graph-confirmed
codec selection, callback/confirmation timeouts, bounded codec exhaustion,
staged link backoff, Busy waits on the final attempt, protocol-stage deadlines,
background exhaustion and cancellation during initial delay or Busy wait,
backoff cancellation and retry budgets surviving graph gaps. Output recovery
checks cover bounded verification, hard failures, release/server loss, paused
players, transient transport activity, and delayed admission when no speaker sink can park the stream. A release regression
covers stale profile callbacks and queued
resumes after handoff, speaker return, preference release, disconnect and Apple
reconnect. Additional regressions cover cancelling the entire Apple reconnect,
retiring callbacks and queued work in every recovery phase, and retaining the
speaker mute when a deferred claim starts during a tick. Existing handoff and
output recovery suites remain in force.
Hardware trials are still needed to establish connection speed and audio sync.

## XPS hardware trial — 2026-11-01

A short trial at 12:39–12:42 PST used the uncommitted `connection-lifecycle`
build on XPS (daemon SHA-256 prefix `ae106f146893`). Gram retained its installed
build, and iPhone Bluetooth stayed enabled. The trial exercised one case-to-ear
connection and one Gram-to-XPS playback takeover.

- Case-to-ear: one control connection attempt, without retry or setup failure.
  The first in-ear report was at 12:40:23.280; BlueZ reported active audio
  transport at 12:40:26.528, about 3.25 seconds later. The user reported that the
  iPhone connected first and XPS followed a few seconds later. The capture does
  not measure the iPhone connection time or first audible sample.
- Gram-to-XPS: XPS received Playing at 12:41:20.239 and performed one output
  rebuild. PipeWire Start completed at 12:41:21.631; only then did FleetPods send
  Play to resume the held player, about 1.39 seconds after the initial Playing
  report. There was no verification retry. The user described the result as
  “seems pretty good.” This is listening feedback, not a measured lip-sync offset.
- Before rollback, the trial daemon remained running with zero automatic service
  restarts, active audio transport and lifecycle state Streaming.

The previous XPS binary was restored after the trial; its service was confirmed
ready and streaming. Gram's installed binary was unchanged. This is limited
hardware evidence for these two paths, not validation of repeated switching,
suspend/resume, microphone behavior or failure recovery on both trial builds.
