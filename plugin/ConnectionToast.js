.pragma library
.import "Model.js" as Model

// One publisher across all per-monitor panels. Only primitive/plain data is
// shared: never retain native notification QObjects in a library's lifetime.
var owner = ""
var serial = 0
var diagnostic = {}
var machine = initial()
// A preview substitutes a scripted input for live status. previewAt is the
// request; demo is the run it started. Both end together, in endPreview.
var previewAt = 0
var demo = 0
function acquire(token) { if (!owner) owner = token; return owner === token }
function release(token) { if (owner === token) { machine.visible = false; previewAt = demo = 0; owner = "" } }
function token() { return "fleetpods-toast-" + (++serial) }
function preview(now) { previewAt = now }
function reset() { machine = initial() }
function endPreview() { previewAt = demo = 0; machine = initial() }
function previewInput(age) {
  return { key: "preview", target: "preview", busy: age < 5000,
    phase: age < 2000 ? "connecting" : age < 4000 ? "preparing" : age < 5000 ? "routing" : "playing" }
}
function advance(input, now, observed, duration, enabled) {
  if (previewAt !== demo) { demo = previewAt; machine = initial() }
  if (demo) input = Object.assign({}, input, previewInput(now - demo))
  machine = step(machine, input, now, observed, duration, enabled)
  var result = presentation(machine, now, duration)
  result.preview = demo !== 0
  // Return to live status as soon as the preview's card closes, so real
  // takeovers immediately become eligible again.
  if (demo && (!machine.visible || now - demo >= PREVIEW_MS)) endPreview()
  return result
}

var LEAD = 500, HOLD = 1000
// Display limits only. LIMIT_MS is also the native card's own expiry, so a
// card this model stops tracking still leaves on the same deadline.
var LIMIT_MS = 30000, WAITING_MS = 10000, PREVIEW_MS = 14000
function initial() {
  return { busy: false, key: "", target: null, started: 0, ended: 0, phase: "",
    visible: false, suppressed: false, artworkStarted: null, input: {} }
}
// Native notification bodies accept markup. Remove markup delimiters (including
// entity syntax) from device labels before handing text to that shared sink.
function label(value) {
  return String(value || "AirPods").slice(0, 100).replace(/[<>]/g, function(c) { return c === "<" ? "‹" : "›" })
    .replace(/&/g, "＆").replace(/[\x00-\x1f\x7f]/g, " ")
}
function project(status, reachable, now) {
  var s = status || {}, life = s.connectionLifecycle || {}, native = s.nativeHandoff || {}
  var state = life.state || ""
  var route = (s.fleet || {}).routing || {}
  var key = (s.accessories || []).filter(function(a) { return a.selected }).map(function(a) { return a.address }).join("")
    || s.deviceName || s.modelName || "pods"
  var success = state === "streaming" && s.audioTransportActive === true
  // Activity can request a background seat without asking for audio; the
  // shared predicate admits only a playback claim.
  var busy = reachable && !success && Model.acquiringPlayback(s)
  var phase = success ? "playing" : state === "routing" ? "routing"
    : state === "preparing_paths" || state === "completing_profiles" ? "preparing" : "connecting"
  if (life.waiting_for === "output_running") phase = "waiting"
  if (state === "failed" || (!busy && !success && (native.failure || route.phase === "failed"))) phase = "failed"
  else if (state === "interrupted" || (!busy && !success)) phase = "cancelled"
  if (!reachable) phase = "offline"
  var levels = (s.isHeadset ? [s.headset] : [s.left, s.right]).filter(function(p) {
    return p && typeof p.level === "number" && p.level >= 0 && p.level <= 100
  }).map(function(p) { return p.level })
  var fresh = s.connected || (Number(s.lastAdvertisementMs) > 0 && now - s.lastAdvertisementMs < 300000)
  // Playback intent survives a fast routing phase and remains set while paused.
  // null means no reliable baseline yet, so shell startup stays quiet.
  var target = reachable && state ? (busy || (s.ownershipIntent === "claim" && success) ? key : "") : null
  return { key: key, target: target, busy: busy, phase: phase, name: label(s.modelName || s.deviceName),
    variant: s.isHeadset ? "max" : "pro", battery: fresh && levels.length ? Math.min.apply(Math, levels) : -1 }
}
// Native attachment/dismissal are observations, not another presentation clock.
// One owner decides the entire lifetime, including the Playing turn and hold.
// A disabled toast keeps its baseline, so enabling it later does not announce
// playback that was already established.
function step(previous, input, now, observed, duration, enabled) {
  observed = observed || {}
  duration = duration === undefined ? 1000 : duration
  var busy = enabled !== false && input.busy
  var s = Object.assign({}, previous)
  // Pausing an established route leaves it waiting for output_running. That
  // observation alone is not a fresh connection attempt.
  var acquired = input.target && s.target !== null && input.target !== s.target
    && (busy || input.phase === "playing")
  var progressing = busy && input.phase !== "waiting"
    && (!s.busy || s.key !== input.key || !s.started)
  var start = (!s.visible || s.key !== input.key) && (acquired || progressing)
  if (start) s = Object.assign(initial(), { key: input.key, started: now, visible: true })
  s.busy = busy
  s.target = input.target === undefined ? null : input.target
  s.input = input
  s.phase = input.phase
  // A completion for a previous publication cannot affect a newer toast.
  var current = observed.episode === undefined || observed.episode === s.started
  if (enabled === false || (current && (observed.dismissed || observed.failed))) s.suppressed = true
  if (s.visible) {
    if (s.key !== input.key) s.suppressed = true
    if (current && observed.attached && input.phase === "playing" && s.artworkStarted === null)
      s.artworkStarted = now
    if (busy) s.ended = 0
    else if (!s.ended || (input.phase === "playing" && previous.phase !== "playing")) s.ended = now
    var deadline = s.ended + (input.phase === "playing" ? HOLD : 3000)
    if (input.phase === "playing" && s.artworkStarted !== null)
      deadline = Math.max(deadline, s.artworkStarted + LEAD + duration + HOLD)
    var settled = s.ended && now >= deadline
    if (s.suppressed || now - s.started >= LIMIT_MS || settled
      || (input.phase === "waiting" && now - s.started >= WAITING_MS)) s.visible = false
  }
  return s
}
function presentation(s, now, duration) {
  var progress = s.artworkStarted === null ? 0 : duration <= 0 ? 1
    : Math.max(0, Math.min(1, (now - s.artworkStarted - LEAD) / duration))
  return { episode: s.started, visible: s.visible, phase: s.phase,
    name: s.input.name || "AirPods", variant: s.input.variant || "max",
    battery: s.input.battery === undefined ? -1 : s.input.battery,
    artworkProgress: progress, animating: s.visible && s.artworkStarted !== null && progress < 1,
    text: text(s.input) }
}
function text(input) {
  var suffix = { connecting: "connecting…", preparing: "preparing audio…", routing: "routing audio…",
    waiting: "connected", playing: "playing", failed: "couldn’t connect", offline: "connection interrupted",
    cancelled: "connection ended" }
  var body = input.phase === "waiting" ? "Waiting for audio" : input.phase === "playing" ? "Audio is playing on your pods"
      : input.phase === "failed" || input.phase === "offline" ? "Check FleetPods for details"
      : input.phase === "cancelled" ? "Audio connection cancelled" : "Connecting audio"
  return { summary: (input.name || "AirPods") + " " + (suffix[input.phase] || "connecting…"), body: body,
    detail: ({connecting: "Connecting…", preparing: "Preparing audio…", routing: "Routing audio…",
      playing: "Playing"})[input.phase] || body }
}
