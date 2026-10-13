// Run with: deno run --allow-read plugin/tests/replay.test.js
// Replays recorded panel sessions through the status log and requires the rows the live shell produced.
// Fixtures come from a recorder build of the plugin, scrubbed by replays/scrub.js; see replays/README.md.
const dir = new URL("./replays/", import.meta.url)
const source = Deno.readTextFileSync(new URL("../Model.js", import.meta.url))
const writeSeats = Deno.args.includes("--write-seats")
const exported = new Function(source +
  "; return { parseStatus, statusLog: typeof statusLog === 'function' ? statusLog : null }")()

// The rows the shell showed, rebuilt from its recorded appends, rewrites, closes and clears, less the rows a
// <name>.changes.json beside the recording drops or rewords on purpose: "drop" and "replace" name one row by time and
// message ("replace" may also change its "kind"), "add" inserts a row the shell missed ({ "t", "kind", "message", "why" }),
// "clear" empties the log where the shell now hides it ({ "t", "why" }), and "rewrite" applies a pattern to every row
// ({ "pattern", "with", "why" }); "amend" updates the latest matching row when a later confirmation arrives.
// "add" and "amend" may set "live"; "start" moves a recorded connection row earlier ({ "t", "message", "at",
// "with", "why" }) while preserving its recorded index for later completion rewrites.
function goldenAfter(events, upTo, changes) {
  // rows is the log as shown; recorded holds only the shell's own rows, which its rewrites and closes index.
  const rows = [], recorded = []
  const dropped = (e) => (changes.drop || []).some((d) => d.t === e.t && d.message === e.message)
  const added = (changes.add || []).map((a) => ({ t: a.t, ev: "added", kind: a.kind, message: a.message, live: a.live === true }))
    .concat((changes.clear || []).map((c) => ({ t: c.t, ev: "cleared" })))
    .concat((changes.amend || []).map((a) => ({ ...a, ev: "amended" })))
  const started = (changes.start || []).map((a) => ({ ...a, originalT: a.t, t: a.at, ev: "started" }))
  const earlyRows = new Map()
  const merged = events.concat(added, started).sort((a, b) => a.t - b.t)
  for (const e of merged) {
    if (e.t > upTo) break
    const reworded = (changes.replace || []).find((r) => r.t === e.t && r.message === e.message)
    let message = reworded ? reworded.with : e.message
    if (message !== undefined) for (const rule of changes.rewrite || []) message = message.replace(new RegExp(rule.pattern), rule.with)
    if (e.ev === "started") {
      const row = { kind: "connection", message: e.with, live: true }
      earlyRows.set(e.originalT, row)
      rows.push(row)
    } else if (e.ev === "row" && earlyRows.has(e.t)) {
      const rule = (changes.start || []).find((a) => a.t === e.t)
      if (rule.message !== e.message) throw new Error("Start rule does not match recorded row at " + e.t)
      recorded.push(earlyRows.get(e.t))
    } else if (e.ev === "row") {
      const row = { kind: (reworded && reworded.kind) || e.kind, message, live: e.live, dropped: dropped(e) }
      rows.push(row); recorded.push(row)
    } else if (e.ev === "amended") {
      const row = rows.findLast((r) => !r.dropped && r.message === e.message)
      if (!row) throw new Error("No row to amend at " + e.t + ": " + e.message)
      Object.assign(row, { message: e.with, kind: e.kind || row.kind })
      if (e.live !== undefined) row.live = e.live
    } else if (e.ev === "added") rows.push({ kind: e.kind, message, live: e.live === true })
    else if (e.ev === "rewrite" && recorded[e.index]) Object.assign(recorded[e.index], { message, live: e.live })
    else if (e.ev === "close" && recorded[e.index]) recorded[e.index].live = false
    else if (e.ev === "clear") { rows.length = 0; recorded.length = 0 }
    // A clear the shell did not make leaves its recorded rows indexable for later rewrites.
    else if (e.ev === "cleared") for (const row of rows) row.dropped = true
  }
  return rows.filter((r) => !r.dropped).map(({ kind, message, live }) => ({ kind, message, live }))
}

// The seats as the panel draws them; recorded only when they change, as <name>.seats.jsonl.
function seatSnapshot(state) {
  const seat = state.seat
  if (!seat) return null
  const labels = (devices) => (devices || []).map((d) => d.label + (d.onAcl === false ? "?" : ""))
  return { apple: labels(seat.seats.apple), omarchy: labels(seat.seats.omarchy), trayApple: labels(seat.availableApple),
    trayOmarchy: labels(seat.availableOmarchy), appleAudio: seat.appleOwnsAudio, omarchyAudio: seat.omarchyOwnsAudio,
    playing: seat.audioPlaying, address: seat.audioDeviceAddress, switching: seat.switching, label: seat.omarchyTransitionLabel,
    linuxHeld: seat.linuxHeld, unknown: seat.viewOwnershipUnknown, connected: seat.connectedToOmarchy,
    appleTray: seat.appleTrayAvailable, subtitle: state.subtitle, showSeats: state.showSeats }
}

let failures = 0
const fixtures = []
for await (const entry of Deno.readDir(dir))
  if (entry.name.endsWith(".jsonl") && !entry.name.endsWith(".seats.jsonl")) fixtures.push(entry.name)
fixtures.sort()
if (!fixtures.length) throw new Error("no replay fixtures in " + dir.pathname)

for (const name of fixtures) {
  const events = (await Deno.readTextFile(new URL(name, dir))).split("\n").filter(Boolean).map((l) => JSON.parse(l))
  const changesUrl = new URL(name.replace(/\.jsonl$/, ".changes.json"), dir)
  const changes = await Deno.readTextFile(changesUrl).then(JSON.parse, () => ({}))
  // Rebuild each full status line from the first one and the changed fields after it.
  let full = null
  for (const e of events) {
    if (e.ev !== "status") continue
    full = e.raw !== undefined ? JSON.parse(e.raw) : { ...full, ...e.set }
    for (const key of e.drop || []) delete full[key]
    e.raw = JSON.stringify(full)
  }
  const leftover = JSON.stringify(events).match(/(?:[0-9A-Fa-f]{2}[:_]){5}[0-9A-Fa-f]{2}/g)
    ?.filter((a) => !/^02[:_]00[:_]00/i.test(a))
  if (leftover?.length) throw new Error(name + ": unscrubbed address")
  if (!exported.statusLog) {
    console.log("FAIL " + name + ": no status log to replay through yet")
    failures++
    continue
  }
  // Compare at each status boundary: the rows just before the next status must match.
  let state = null
  let statusIndex = -1
  const seats = []
  const step = (event) => {
    state = exported.statusLog(state, event)
    const snap = seatSnapshot(state)
    const line = snap && JSON.stringify({ t: event.t, ...snap })
    if (snap && (!seats.length || JSON.stringify({ ...JSON.parse(seats[seats.length - 1]), t: event.t }) !== line)) seats.push(line)
  }
  let logFailed = false
  for (let i = 0; i < events.length; ++i) {
    const e = events[i]
    if (e.ev === "status") ++statusIndex
    // The panel's timers fire on their own, between recorded events.
    while (state && state.wakeAt && state.wakeAt <= e.t) step({ t: state.wakeAt, ev: "tick" })
    step(e.ev === "status" ? { ...e, status: exported.parseStatus(e.raw) } : e)
    if (logFailed) continue
    const next = events[i + 1]
    if (next && next.ev !== "status") continue
    const want = goldenAfter(events, next ? Math.max(e.t, next.t - 1) : Infinity, changes).map((r) => JSON.stringify(r))
    const got = state.rows.map((r) => JSON.stringify({ kind: r.kind, message: r.message, live: r.live }))
    if (want.join() !== got.join()) {
      let at = 0
      while (at < want.length && want[at] === got[at]) ++at
      console.log("FAIL " + name + " at t=" + e.t + " (status " + statusIndex + "), rows from " + at + ":\n  expected "
        + want.slice(at).join("\n           ") + "\n  got      " + got.slice(at).join("\n           "))
      failures++
      logFailed = true
    }
  }
  const seatsUrl = new URL(name.replace(/\.jsonl$/, ".seats.jsonl"), dir)
  if (writeSeats) { await Deno.writeTextFile(seatsUrl, seats.join("\n") + "\n"); continue }
  const wantSeats = (await Deno.readTextFile(seatsUrl).catch(() => "")).split("\n").filter(Boolean)
  let at = 0
  while (at < wantSeats.length && at < seats.length && wantSeats[at] === seats[at]) ++at
  if (at < wantSeats.length || at < seats.length) {
    console.log("FAIL " + name + " seats, change " + at + ":\n  expected " + (wantSeats[at] || "(none)") + "\n  got      " + (seats[at] || "(none)"))
    failures++
  }
}
if (failures) {
  console.log("replay.test.js: " + failures + " failing")
  Deno.exit(1)
}
console.log("replay.test.js: " + fixtures.length + " sessions replayed")
