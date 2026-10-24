// No QML imports on purpose, so every function here runs in a plain JS harness.

var NOISE_OFF = 0
var NOISE_ANC = 1
var NOISE_TRANSPARENCY = 2
var NOISE_ADAPTIVE = 3

var LID_OPEN = 0
var LID_CLOSED = 1
var LID_UNKNOWN = 2

var TELEMETRY_EXPIRY_MS = 120000
var AUDIO_CARRY_MS = 15000

var EAR_PAUSE_ONE_OUT = 0
var EAR_PAUSE_BOTH_OUT = 1
var EAR_DISABLED = 2

// Highest schema_version this panel knows how to read.
var SUPPORTED_SCHEMA = 1

// Level the daemon reports when a pod or the case has not been heard from.
var LEVEL_UNKNOWN = -1

// noise_mode before the daemon has identified the device.
var NOISE_UNKNOWN = -1

// How many ear-detection behaviours exist, so callers can cycle without a bare 3.
var EAR_BEHAVIOR_COUNT = 3

// nf-md-check U+F012C, the one candidate glyph that rendered on the box.
var GLYPH_CHECK = "\uDB80\uDD2C"

// control_connection.stage while this laptop's link is being set up.
var CONTROL_OPENING_STAGES = ["connecting", "handshake", "features", "notifications", "retrying"]
// fleet.routing.phase while this laptop's own request is in flight; the other busy phases admit another laptop's.
var ROUTE_REQUESTING_PHASES = ["waiting_admission", "connecting", "waiting_completion"]
// fleet.routing.phase once the helper has admitted this laptop's request.
var ROUTE_ADMITTED_PHASES = ["connecting", "waiting_completion", "success"]
// fleet_outcome.result values that end a route without a seat change.
var ROUTE_FAILURES = ["peer_busy", "peer_unavailable", "join_failed", "admit_failed"]

// Longest error the panel will show inside a row, and the cut that leaves room for the ellipsis.
var MAX_ERROR_CHARS = 140
var ELIDED_ERROR_CHARS = 137

function defaultPod() {
  return { level: LEVEL_UNKNOWN, charging: false, inEar: false, inCase: false, outOfCase: false }
}

// Full shape on every path, so the panel never reads undefined off a parse failure.
function defaultStatus() {
  return {
    ok: false,
    lastError: "",
    schemaVersion: 0,
    schemaTooNew: false,
    connected: false,
    earReported: false,
    nativeHandoff: null,
    controlConnection: null,
    reportedOwnership: -1,
    audioSourceLocal: -1,
    audioSourceType: -1,
    audioOwner: { holder: "", playing: false },
    remotePlaying: false,
    audioTransportActive: false,
    ownershipIntent: "none",
    remotePeerCount: -1,
    handoffResult: "none",
    ownershipUpdated: 0,
    sourceUpdated: 0,
    peerListUpdated: 0,
    deviceName: "",
    modelName: "",
    modelInt: -1,
    isProSeries: false,
    isHeadset: false,
    supportsNoiseOff: true,
    supportsNoiseControl: true,
    supportsAdaptive: false,
    supportsConversationalAwareness: false,
    supportsOneBudANC: false,
    noiseMode: NOISE_UNKNOWN,
    adaptiveNoiseLevel: 0,
    oneBudANC: false,
    conversationalAwareness: false,
    earDetectionBehavior: EAR_PAUSE_ONE_OUT,
    lidState: LID_UNKNOWN,
    bleConnectionState: 255,
    caseConfirmedDocked: false,
    waitingForPlayback: false,
    caseLidState: LID_UNKNOWN,
    lidUpdatedMs: 0,
    devices: {},
    fleetOutcome: null,
    fleetWaitingForKeyring: false,
    left: defaultPod(),
    right: defaultPod(),
    caseBattery: { level: LEVEL_UNKNOWN, charging: false },
    headset: { level: LEVEL_UNKNOWN, charging: false },
    // An older daemon omits the key. Absence is not "still learning".
    pairingKeysReady: true,
    bluetoothPaired: true,
    bluetoothBonded: null,
    pairingMode: false,
    pairingCandidates: 0,
    bluezConnected: false
  }
}

// The dotted case is the Pro drawing. An unknown model uses it too. A known non-Pro stays on the outline.
function iconVariant(isHeadset, isProSeries, modelInt) {
  if (isHeadset) return "max"
  if (isProSeries) return "pro"
  if (modelInt === 0) return "unknown"
  return "buds"
}

function pairingWarning(status) {
  return status.daemonReachable && status.bluetoothPaired && status.bluetoothBonded === false
    ? "Pairing hasn’t been saved. Put your AirPods in pairing mode and pair them again in the Bluetooth plugin before using Reconnect."
    : ""
}

function emptyGuidance(daemonReachable, pairingKeysReady, modelInt, nearby, pairingMode, pairingCandidates, caseReady) {
  if (!daemonReachable)
    return "Start the FleetPods service to see the Apple and Omarchy seats."
  if (!pairingKeysReady) {
    if (pairingCandidates > 1)
      return "More than one pair of AirPods is ready to pair. Select your AirPods in the Bluetooth plugin."
    if (pairingMode)
      return "Your AirPods are ready to pair. Select them in the Bluetooth plugin."
    if ([7, 8, 12].indexOf(modelInt) >= 0)
      return "Pair your AirPods in the Bluetooth plugin."
    if (!nearby || caseReady !== true)
      return "Open the case lid with both AirPods inside."
    if ([9, 10, 11].indexOf(modelInt) >= 0)
      return "Double-tap the front of the case until the light flashes white, then select your AirPods in the Bluetooth plugin."
    if ([1, 2, 3, 4, 5, 6].indexOf(modelInt) >= 0)
      return "Hold the button on the back of the case until the light flashes white, then select your AirPods in the Bluetooth plugin."
    return "Put your AirPods in pairing mode, then select them in the Bluetooth plugin."
  }
  return "Open the lid on the AirPods case to allow Bluetooth connections to the pods."
}

// The closed-case screen hides the console. Clear the log so the next connection starts empty.
function statusLogSuppressed(guidanceVisible, subtitleKind) {
  return guidanceVisible === true && (subtitleKind === "inCase" || subtitleKind === "caseClosed" || subtitleKind === "waiting" || subtitleKind === "pairingRequired")
}

function controlOpening(control) {
  return !!control && CONTROL_OPENING_STAGES.indexOf(control.stage) >= 0
}

function routeRequesting(routing) {
  return !!(routing && routing.busy && ROUTE_REQUESTING_PHASES.indexOf(routing.phase) >= 0)
}

function routeAdmitted(routing) {
  return !!routing && ROUTE_ADMITTED_PHASES.indexOf(routing.phase) >= 0
}

function openingControlLink(connected, control, routing) {
  if (connected) return false
  return controlOpening(control) || routeRequesting(routing)
}

// The subtitle names the seat the panel draws lit, so the two never disagree. Both lit defers to the lit device.
function seatAudioSubtitle(omarchyOwns, appleOwns, memberLabel, memberKind, playing) {
  var fleet = omarchyOwns && appleOwns ? (memberKind === "apple" ? "Apple" : "Omarchy")
    : omarchyOwns ? "Omarchy" : appleOwns ? "Apple" : ""
  if (!fleet) return ""
  var member = memberKind === fleet.toLowerCase() ? memberLabel : ""
  return playing ? playingLabel(member, fleet) : fleetLabel(fleet, member)
}

function heldSubtitle(switching, live, lastAudio) {
  return switching && lastAudio ? lastAudio : live
}

function connectedFleetPeer(status) {
  if (!status.connected || !status.controlConnection || status.controlConnection.stage !== "ready"
      || !(status.peerListUpdated > 0)) return ""
  var local = normalizeAddress(status.localAddress)
  var peers = addressList(status.connectedPeers)
  if (peers.length !== 2 || peers.indexOf(local) < 0) return ""
  var other = peers[0] === local ? peers[1] : peers[0]
  var info = peerInfo(other, local, status.localName, status.devices)
  return info.kind === "omarchy" && !info.discovered ? other : ""
}

function observedFleetJoin(previous, status) {
  var peer = connectedFleetPeer(status)
  if (!previous.ok || !peer || connectedFleetPeer(previous) === peer) return null
  var localName = hostLabel(status.localName)
  var peerName = peerInfo(peer, status.localAddress, status.localName, status.devices).label
  var alreadyHere = previous.connected && previous.controlConnection && previous.controlConnection.stage === "ready"
  return { kind: "pairedAlongside", device: alreadyHere ? peerName : localName, peer: alreadyHere ? localName : peerName }
}

// The fleet transaction that ended since the previous status, from the daemon's fleet_outcome. A daemon without that
// field is read from its routing phase leaving busy, which misses a transaction that ended between two status lines.
function fleetOutcome(previous, status) {
  if (status.fleetOutcome) {
    var before = previous.fleetOutcome
    return previous.ok && !(before && before.atMs === status.fleetOutcome.atMs) ? status.fleetOutcome : null
  }
  var asked = previous.fleet && previous.fleet.routing, routing = status.fleet && status.fleet.routing
  if (!asked || !routing || asked.busy !== true || (routing.phase !== "success" && routing.phase !== "failed")) return null
  var ok = routing.phase === "success"
  var peer = normalizeAddress(ok ? routing.peer : asked.peer), retain = normalizeAddress(routing.retain)
  var result = ROUTE_REQUESTING_PHASES.indexOf(asked.phase) >= 0
    ? (ok ? (peer && retain === peer ? "joined_alongside" : "joined")
      : routing.error === "peer_busy" ? "peer_busy" : routing.error === "control_unavailable" ? "peer_unavailable" : "join_failed")
    : (ok ? (status.localAddress && retain === normalizeAddress(status.localAddress) ? "admitted_alongside" : "admitted")
      : "admit_failed")
  return { atMs: 0, result: result, peer: peer, retain: retain }
}

function fleetRouteFailed(outcome) {
  return !!outcome && ROUTE_FAILURES.indexOf(outcome.result) >= 0
}

// Desktop activity taking the Omarchy seat carries no audio claim.
function backgroundJoin(status) {
  if (status.ownershipIntent === "claim") return false
  var control = status.controlConnection || {}
  if (routeRequesting(status.fleet && status.fleet.routing)) return true
  if (control.recovering) return false
  return controlOpening(control)
}

// A blank between two copies of a caption is not a new log line.
function statusCaptionChanged(lastLogged, next) {
  var message = String(next || "").trim()
  return message !== "" && message !== String(lastLogged || "").trim()
}

function intOr(value, fallback) {
  var n = parseInt(value, 10)
  return isFinite(n) ? n : fallback
}

// A daemon older than the capability keys sends nothing, which is not the same as false.
function boolOr(value, fallback) {
  return value === undefined ? fallback : value === true
}

function podFrom(raw) {
  var pod = defaultPod()
  if (!raw || typeof raw !== "object") return pod
  // available:false means the daemon has stopped hearing from this pod, so its charging and in_ear are stale too.
  if (raw.available !== true) return pod
  pod.level = intOr(raw.level, LEVEL_UNKNOWN)
  pod.charging = raw.charging === true
  pod.inEar = raw.in_ear === true
  pod.inCase = raw.in_case === true
  // Missing placement is unknown, not evidence that a pod has left its well.
  pod.outOfCase = raw.in_case === false
  return pod
}

// The whole of $XDG_STATE_HOME/fleetpods/status.json, one line, keys sorted by QJsonObject rather than by insert order:
// {"adaptive_level_changes_total":0,"adaptive_noise_level":50,"ca_changes_total":0,
//  "case":{"available":true,"charging":false,"level":100},"connect_calls_total":0,
//  "connect_failures_total":0,"connected":true,"conversational_awareness":true,
//  "device_name":"AirPods Pro","disconnect_calls_total":0,
//  "disconnect_failures_total":0,"ear_detection_behavior":0,
//  "ear_detection_changes_total":0,"forget_calls_total":0,"is_pro_series":true,
//  "left":{"available":true,"charging":false,"in_ear":false,"level":79},
//  "lid_state":2,"model_int":11,"model_name":"AirPods Pro 3","model_number":"A3064",
//  "noise_control_changes_total":0,"noise_mode":1,"one_bud_anc_changes_total":0,
//  "one_bud_anc_mode":true,"reconnect_attempts_total":0,"reconnect_failures_total":0,
//  "reopen_calls_total":0,
//  "right":{"available":true,"charging":true,"in_ear":false,"level":100},
//  "schema_version":1,"supports_adaptive":true,"supports_conversational_awareness":true,
//  "supports_noise_control":true,"supports_noise_off":false,"supports_one_bud_anc":true}
// A headset (AirPods Max) instead sends "is_headset":true and "headset":{"available":true,"charging":false,"level":100}, with no pods and no case.
function parseStatus(raw) {
  var status = defaultStatus()
  var text = String(raw || "").trim()
  if (text === "") {
    status.lastError = "The FleetPods status file is empty"
    return status
  }

  var parsed
  try {
    parsed = JSON.parse(text)
  } catch (e) {
    status.lastError = "Could not read the FleetPods status file"
    return status
  }
  if (!parsed || typeof parsed !== "object" || parsed.schema_version === undefined) {
    status.lastError = "The FleetPods status file carried no schema_version"
    return status
  }

  status.schemaVersion = intOr(parsed.schema_version, 0)
  if (status.schemaVersion > SUPPORTED_SCHEMA) {
    // Newer daemon: report the version rather than draw fields we may be misreading.
    status.schemaTooNew = true
    status.lastError = "FleetPods speaks status schema " + status.schemaVersion + ", this panel reads " + SUPPORTED_SCHEMA
    return status
  }

  status.ok = true
  status.connected = parsed.connected === true
  // Older daemons omit the validity flag, so their pod values remain usable.
  status.earReported = parsed.ear_reported !== false
  status.controlConnection = parsed.control_connection || null
  status.reportedOwnership = [0, 1].indexOf(parsed.reported_ownership) >= 0 ? parsed.reported_ownership : -1
  status.audioSourceLocal = [0, 1].indexOf(parsed.audio_source_local) >= 0 ? parsed.audio_source_local : -1
  status.audioSourceType = [0, 1, 2].indexOf(parsed.audio_source_type) >= 0 ? parsed.audio_source_type : -1
  var owner = parsed.audio_owner || {}
  status.audioOwner = { holder: ["local", "remote"].indexOf(owner.holder) >= 0 ? owner.holder : "", playing: owner.playing === true }
  status.audioTransportActive = parsed.audio_transport_active === true
  status.remotePlaying = parsed.remote_playing === true
  status.nativeHandoff = parsed.native_handoff && parsed.native_handoff.enabled === true ? parsed.native_handoff : null
  status.ownershipIntent = String(parsed.ownership_intent || "none")
  status.remotePeerCount = intOr(parsed.remote_peer_count, -1)
  status.handoffResult = String(parsed.handoff_result || "none")
  status.ownershipUpdated = Number(parsed.ownership_updated_ms || 0)
  status.sourceUpdated = Number(parsed.source_updated_ms || 0)
  status.peerListUpdated = Number(parsed.peer_list_updated_ms || 0)
  status.deviceName = String(parsed.device_name || "")
  status.modelName = String(parsed.model_name || "")
  status.modelInt = intOr(parsed.model_int, -1)
  status.isProSeries = parsed.is_pro_series === true
  // Older daemons send neither key, so this stays false and the pod rows keep drawing.
  status.isHeadset = parsed.is_headset === true
  // Older daemons do not send this, and every model before the Pro 3 had Off.
  status.supportsNoiseOff = parsed.supports_noise_off !== false
  // A daemon without these keys falls back to what the panel used to gate on.
  status.supportsNoiseControl = boolOr(parsed.supports_noise_control, true)
  status.supportsAdaptive = boolOr(parsed.supports_adaptive, status.isProSeries)
  status.supportsConversationalAwareness = boolOr(parsed.supports_conversational_awareness, status.isProSeries)
  status.supportsOneBudANC = boolOr(parsed.supports_one_bud_anc, status.isProSeries)
  status.noiseMode = intOr(parsed.noise_mode, NOISE_UNKNOWN)
  status.adaptiveNoiseLevel = intOr(parsed.adaptive_noise_level, 0)
  status.oneBudANC = parsed.one_bud_anc_mode === true
  status.conversationalAwareness = parsed.conversational_awareness === true
  status.earDetectionBehavior = intOr(parsed.ear_detection_behavior, EAR_PAUSE_ONE_OUT)
  status.lidState = intOr(parsed.lid_state, LID_UNKNOWN)
  status.bleConnectionState = intOr(parsed.ble_connection_state, 255)
  status.caseConfirmedDocked = parsed.case_confirmed_docked === true
  status.waitingForPlayback = parsed.waiting_for_playback === true
  status.caseLidState = intOr(parsed.last_known_lid_state, status.lidState)
  status.lastAdvertisementMs = Number(parsed.last_advertisement_ms || 0)
  status.lidUpdatedMs = Number(parsed.lid_updated_ms === undefined
    ? status.lastAdvertisementMs : parsed.lid_updated_ms)
  status.connectedPeers = addressList(parsed.connected_peers)
  status.localAddress = normalizeAddress(parsed.local_address)
  status.localName = String(parsed.local_name || "")
  status.remoteSourceAddress = normalizeAddress(parsed.remote_source_address)
  status.rosterAudioOwner = normalizeAddress(parsed.roster_audio_owner)
  status.pairingKeysReady = boolOr(parsed.pairing_keys_ready, true)
  status.bluetoothPaired = boolOr(parsed.bluetooth_paired, true)
  status.bluetoothBonded = typeof parsed.bluetooth_bonded === "boolean" ? parsed.bluetooth_bonded : null
  status.pairingMode = parsed.pairing_mode === true
  status.pairingCandidates = intOr(parsed.pairing_candidates, 0)
  status.bluezConnected = parsed.bluez_connected === true
  status.wantOmarchySeat = parsed.want_omarchy_seat === true
  status.holdingOmarchySeat = parsed.holding_omarchy_seat === true
  status.fleet = parsed.fleet && typeof parsed.fleet === "object" ? parsed.fleet : { phase: "idle" }
  status.fleetWaitingForKeyring = status.fleet.waiting_for_keyring === true
  var outcome = parsed.fleet_outcome
  status.fleetOutcome = outcome && typeof outcome === "object" && outcome.at_ms > 0 && outcome.result
    ? { atMs: outcome.at_ms, result: String(outcome.result), peer: normalizeAddress(outcome.peer),
        retain: normalizeAddress(outcome.retain) } : null
  status.devices = devicesMap(parsed.devices)
  status.left = podFrom(parsed.left)
  status.right = podFrom(parsed.right)
  var caseRaw = podFrom(parsed["case"])
  status.caseBattery = { level: caseRaw.level, charging: caseRaw.charging }
  var headsetRaw = podFrom(parsed.headset)
  status.headset = { level: headsetRaw.level, charging: headsetRaw.charging }
  return status
}

// The one list both the cycle and the panel rows are built from, in the order the panel draws them.
function availableModes(hasNoiseControl, hasOff, hasAdaptive) {
  // AirPods 1, 2, 3 and the plain AirPods 4 have no listening modes at all.
  if (!hasNoiseControl) return []
  var modes = []
  if (hasOff) modes.push(NOISE_OFF)
  modes.push(NOISE_TRANSPARENCY)
  if (hasAdaptive) modes.push(NOISE_ADAPTIVE)
  modes.push(NOISE_ANC)
  return modes
}

function noiseModeName(mode) {
  if (mode === NOISE_OFF) return "Off"
  if (mode === NOISE_ANC) return "Noise Cancellation"
  if (mode === NOISE_TRANSPARENCY) return "Transparency"
  if (mode === NOISE_ADAPTIVE) return "Adaptive"
  return "Unknown"
}

// The four AAP verbs, indexed by mode, in the order the fleetpods client accepts them.
function noiseModeVerb(mode) {
  var verbs = ["noise:off", "noise:anc", "noise:transparency", "noise:adaptive"]
  if (mode < 0 || mode >= verbs.length) return ""
  return verbs[mode]
}

function earDetectionVerb(behavior) {
  var verbs = ["ear:one", "ear:both", "ear:off"]
  if (behavior < 0 || behavior >= verbs.length) return ""
  return verbs[behavior]
}

function earDetectionName(behavior) {
  if (behavior === EAR_PAUSE_ONE_OUT) return "Pause when one is out"
  if (behavior === EAR_PAUSE_BOTH_OUT) return "Pause when both are out"
  if (behavior === EAR_DISABLED) return "Never pause"
  return "Unknown"
}

function levelText(level) {
  return level === LEVEL_UNKNOWN ? "--" : String(level) + "%"
}

// 0 to 1 for the meter; an unknown level draws an empty track rather than a full one.
function levelFraction(level) {
  if (level === LEVEL_UNKNOWN) return 0
  return Math.max(0, Math.min(100, level)) / 100
}

// BLE silence expires the display, not the daemon's ownership or lid state.
function telemetryUnavailable(connected, lastAdvertisementMs, nowMs) {
  var last = Number(lastAdvertisementMs || 0)
  var now = Number(nowMs || 0)
  // No advertisement yet is unknown. It is not evidence that the pods are in the case.
  if (last <= 0) return false
  return !connected && now > 0 && now - last >= TELEMETRY_EXPIRY_MS
}

// A charge from a case that has gone quiet is not a live reading. The rings
// leave the panel until a nearby advertisement brings a level back.
function displayedBatteryLevel(level, daemonReachable, connected, lastAdvertisementMs, nowMs) {
  if (!daemonReachable || telemetryUnavailable(connected, lastAdvertisementMs, nowMs))
    return LEVEL_UNKNOWN
  return level
}

// A confirmed shut, docked case stays known while it stops broadcasting.
function confirmedInCase(lidState, left, right) {
  if (lidState !== LID_CLOSED || !left || !right || left.inEar || right.inEar) return false
  return !!((left.charging || left.inCase) && (right.charging || right.inCase))
}

function podsInClosedCase(status) {
  var left = status.left || status.leftPod || {}, right = status.right || status.rightPod || {}
  var lid = status.iconLidState !== undefined ? status.iconLidState
    : status.caseLidState !== undefined && status.caseLidState !== LID_UNKNOWN ? status.caseLidState : status.lidState
  return !status.isHeadset && lid === LID_CLOSED && !left.inEar && !right.inEar
    && (status.caseConfirmedDocked === true || confirmedInCase(lid, left, right))
}

function showDisconnectedMark(connected, left, right, lastAdvertisementMs, nowMs) {
  var freshReportMs = 20000
  var last = Number(lastAdvertisementMs || 0)
  var age = Number(nowMs || 0) - last
  if (connected || last <= 0 || age < 0 || age >= freshReportMs) return false
  // A noncharging bud can still be docked; an in-ear report confirms it is out.
  return !!((left && left.inEar) || (right && right.inEar))
}

function elsewhereDevice(pods) {
  var address = normalizeAddress(pods.remoteSourceAddress)
  var peer = address && pods.devices && pods.devices[address]
  return peer && peer.kind === "apple" ? peer.label || "" : ""
}

function connectionElsewhere(connected, bleState, lastAdvertisementMs, nowMs, disconnectedMs, bluezConnected) {
  var freshReportMs = 20000
  var last = Number(lastAdvertisementMs || 0)
  var age = Number(nowMs || 0) - last
  // bluez_connected means this computer holds the ACL. The BLE byte does not name a host.
  if (connected || bluezConnected || last <= 0 || last <= Number(disconnectedMs || 0)
      || age < 0 || age >= freshReportMs) return false
  // BleInfo::ConnectionState: idle, music, call, ringing, hanging up.
  return [4, 5, 6, 7, 9].indexOf(bleState) !== -1
}

function inCase(lidState, left, right, caseBattery, lastAdvertisementMs, nowMs, nearbyMs) {
  return !budsOut(lidState, left, right, lastAdvertisementMs, nowMs, false, nearbyMs, caseBattery)
}

function budsOut(lidState, left, right, lastAdvertisementMs, nowMs, connected, nearbyMs, caseBattery) {
  left = left || defaultPod()
  right = right || defaultPod()
  if (left.inEar || right.inEar) return true
  if (lidState === LID_CLOSED) return false
  var caseKnown = caseBattery && caseBattery.level !== LEVEL_UNKNOWN
  if (caseKnown && left.charging && right.charging) return false
  nearbyMs = nearbyMs || 20000
  var last = Number(lastAdvertisementMs || 0)
  var now = Number(nowMs || 0)
  var fresh = last > 0 && now > 0 && now - last < nearbyMs
  if (connected) return true
  if (fresh && lidState === LID_OPEN) return true
  return false
}

// Remember physical placement independently of the delayed artwork transition.
// Not-in-ear alone does not tell us whether a pod has returned to its well.
function rememberPodPlacement(pod, previous) {
  if (pod && (pod.inEar || pod.outOfCase === true)) return false
  if (pod && pod.inCase === true) return true
  return previous === true || previous === false ? previous : null
}

function caseIconState(isHeadset, lidState, left, right, previousState, hadInEar,
                       lastAdvertisementMs, nowMs, nearbyMs, connected, earReported, unpaired,
                       rememberedPlacement) {
  left = left || defaultPod()
  right = right || defaultPod()
  if (isHeadset) return ""
  // Setup starts with a case illustration. An unrelated worn advertisement
  // without case evidence must not turn it into a buds-out illustration.
  if (unpaired === true && lidState === LID_UNKNOWN) return "closed"
  // No advertisement and no connection means there is no worn state to preserve on startup.
  if (connected === false && Number(lastAdvertisementMs) === 0) return "closed"
  if (telemetryUnavailable(connected, lastAdvertisementMs, nowMs)) return "closed"
  var leftOut = rememberPodPlacement(left, rememberedPlacement && rememberedPlacement.left) === false
  var rightOut = rememberPodPlacement(right, rememberedPlacement && rememberedPlacement.right) === false
  // Disconnect clears the ear flags without reporting a physical change.
  // An explicit case-placement report can arrive before any ear report.
  if (!rememberedPlacement && earReported === false
      && !left.outOfCase && !right.outOfCase && !left.inCase && !right.inCase)
    return previousState
  // A shut case stops advertising. Do not wait for the audio link to drop.
  // Silence while a bud is still reported out is not a new closed-lid report.
  var last = Number(lastAdvertisementMs || 0)
  var now = Number(nowMs || 0)
  var stale = now > 0 && last > 0 && now - last >= (nearbyMs || 20000)
  if (stale && !leftOut && !rightOut)
    return "closed"
  if (!connected && stale && (left.charging || right.charging))
    return "closed"
  // This is placement evidence. The artwork owns any empty-case hold and
  // its exit; presentation history must not be fed back into physical state.
  if (leftOut && rightOut) return ""
  // Named for the pod still sitting in the case. Unknown placement retains
  // each pod's last known side of the rim; no history defaults to seated.
  if (leftOut && !rightOut) return "open-right"
  if (rightOut && !leftOut) return "open-left"
  if (lidState === LID_CLOSED) return "closed"
  if (lidState === LID_OPEN) return "open"
  // Lid ads from while the buds were worn describe the empty case, so this tick
  // is UNKNOWN until a new packet. Taking pods out of the ears means the case is open.
  if (previousState === "open" || previousState === "open-left"
    || previousState === "open-right" || previousState === "open-empty" || hadInEar)
    return "open"
  if (previousState === "closed") return "closed"
  return "closed"
}

function lidText(lidState) {
  if (lidState === LID_OPEN) return "Open"
  if (lidState === LID_CLOSED) return "Closed"
  return ""
}

function podMeta(pod) {
  if (pod.charging) return "Charging"
  if (pod.inEar) return "In ear"
  return ""
}

// Collapse the client stderr into one line the panel can show inside a row.
function normalizeAddress(value) {
  return String(value || "").replace(/-/g, ":").toUpperCase()
}

function addressList(value) {
  if (!value || !value.length) return []
  var out = []
  for (var i = 0; i < value.length; i++) out.push(normalizeAddress(value[i]))
  return out
}

function devicesMap(list) {
  var map = {}
  if (!list) return map
  if (!Array.isArray(list)) {
    for (var key in list) {
      var row = list[key] || {}
      var id = normalizeAddress(row.address || key)
      if (!id) continue
      map[id] = { label: row.label || id, kind: row.kind || "unknown" }
      if (row.model) map[id].model = row.model
      if (row.discovered === true) map[id].discovered = true
      if (typeof row.fleet_enrolled === "boolean") map[id].fleetEnrolled = row.fleet_enrolled
      if (typeof row.nearby === "boolean") map[id].nearby = row.nearby
    }
    return map
  }
  for (var i = 0; i < list.length; i++) {
    var item = list[i] || {}
    var addr = normalizeAddress(item.address)
    if (!addr) continue
    map[addr] = { label: item.label || addr, kind: item.kind || "unknown" }
    if (item.model) map[addr].model = item.model
    if (item.discovered === true) map[addr].discovered = true
    if (typeof item.fleet_enrolled === "boolean") map[addr].fleetEnrolled = item.fleet_enrolled
    if (typeof item.nearby === "boolean") map[addr].nearby = item.nearby
  }
  return map
}

function hostLabel(name) {
  return String(name || "").trim() || "Omarchy"
}

function peerInfo(address, localAddress, localName, extraNames) {
  var id = normalizeAddress(address)
  var names = extraNames || {}
  if (id && id === normalizeAddress(localAddress)) {
    return { address: id, label: hostLabel(localName), kind: "omarchy" }
  }
  if (names[id]) {
    var peer = { address: id, label: names[id].label || id, kind: names[id].kind || "unknown" }
    if (names[id].discovered === true) peer.discovered = true
    if (typeof names[id].fleetEnrolled === "boolean") peer.fleetEnrolled = names[id].fleetEnrolled
    if (typeof names[id].nearby === "boolean") peer.nearby = names[id].nearby
    return peer
  }
  return { address: id, label: "Unknown", kind: "unknown" }
}

function knownRoster(extraNames) {
  var names = extraNames || {}
  var out = []
  var seen = {}
  function add(addr) {
    var id = normalizeAddress(addr)
    if (!id || seen[id]) return
    seen[id] = true
    out.push(peerInfo(id, "", "", extraNames))
  }
  for (var extra in names) add(extra)
  return out
}

function isKnownApplePeer(address, extraNames) {
  var id = normalizeAddress(address)
  if (!id) return false
  var names = extraNames || {}
  var device = names[id]
  if (!device || device.kind !== "apple") return false
  var label = String(device.label || "").trim()
  return !!device.model || (!!label && label !== "Unknown" && label !== "Apple device"
    && normalizeAddress(label) !== id)
}

function firstApplePeer(connectedPeers, extraNames, remoteSourceAddress) {
  var peers = connectedPeers || []
  for (var i = 0; i < peers.length; i++) {
    if (isKnownApplePeer(peers[i], extraNames)) return normalizeAddress(peers[i])
  }
  var source = normalizeAddress(remoteSourceAddress)
  if (source) {
    if (isKnownApplePeer(source, extraNames)) return source
  }
  return ""
}

function appleOccupantAddress(connectedPeers, extraNames, remoteSourceAddress) {
  var source = normalizeAddress(remoteSourceAddress)
  if (isKnownApplePeer(source, extraNames) && (connectedPeers || []).indexOf(source) !== -1) return source
  return firstApplePeer(connectedPeers, extraNames, remoteSourceAddress)
}

function applePadDevices(connectedPeers, extraNames, remoteSourceAddress) {
  var address = appleOccupantAddress(connectedPeers, extraNames, remoteSourceAddress)
  if (!address) return []
  var device = peerInfo(address, "", "", extraNames)
  device.onAcl = (connectedPeers || []).indexOf(address) !== -1
  return [device]
}

function availableDevices(kind, connectedPeers, extraNames, localAddress, remoteSourceAddress, localOnAcl) {
  var seated = {}
  var local = normalizeAddress(localAddress)
  if (local && (kind !== "omarchy" || localOnAcl !== false)) seated[local] = true
  if (kind === "omarchy") {
    var peers = connectedPeers || []
    for (var i = 0; i < peers.length; i++) {
      var peer = peerInfo(peers[i], local, "", extraNames)
      if (peer.kind === "omarchy" && !peer.discovered) seated[peer.address] = true
    }
  } else {
    var seatedApple = applePadDevices(connectedPeers, extraNames, remoteSourceAddress)
    for (var s = 0; s < seatedApple.length; s++) seated[seatedApple[s].address] = true
  }
  var out = []
  var roster = knownRoster(extraNames)
  for (var j = 0; j < roster.length; j++) {
    if (roster[j].kind !== kind) continue
    if (seated[roster[j].address]) continue
    if (kind === "omarchy" && roster[j].discovered && roster[j].nearby === false
        && (connectedPeers || []).indexOf(roster[j].address) === -1) continue
    // Old scans saved labels like "02-00-00-00-00-04" as Apple devices.
    var label = String(roster[j].label || "").trim()
    if (kind === "apple" && (!label || label === "Apple device" || label === "Unknown"
        || normalizeAddress(label) === roster[j].address)) continue
    out.push(roster[j])
  }
  out.sort(function (a, b) {
    var discoveryOrder = Number(!!b.discovered) - Number(!!a.discovered)
    if (discoveryOrder || kind !== "apple") return discoveryOrder
    return Number(!!extraNames[b.address].model) - Number(!!extraNames[a.address].model)
  })
  return out
}

function seatOccupants(connectedPeers, localAddress, localName, extraNames, linuxOnAcl, remoteSourceAddress) {
  var apple = []
  var omarchy = []
  var peers = connectedPeers || []
  var local = normalizeAddress(localAddress)
  var names = Object.assign({}, extraNames || {})
  if (local) names[local] = { label: hostLabel(localName), kind: "omarchy" }
  apple = applePadDevices(connectedPeers, names, remoteSourceAddress)
  for (var i = 0; i < peers.length; i++) {
    var info = peerInfo(peers[i], local, localName, extraNames)
    info.onAcl = true
    if (info.kind === "omarchy" && !info.discovered) omarchy.push(info)
  }
  if (local && linuxOnAcl !== false) {
    var self = peerInfo(local, local, localName, extraNames)
    self.onAcl = linuxOnAcl === true
    var found = false
    for (var j = 0; j < omarchy.length; j++) {
      if (omarchy[j].address === local) { omarchy[j].onAcl = self.onAcl; found = true }
    }
    if (!found) omarchy.push(self)
  }
  return { apple: apple, omarchy: omarchy }
}

function podsNearby(lastAdvertisementMs, lidState, nowMs, nearbyMs) {
  if (lidState === LID_CLOSED) return false
  var last = Number(lastAdvertisementMs || 0)
  if (last <= 0) return false
  return (nowMs || Date.now()) - last < (nearbyMs || 20000)
}

function controlConnectionLabel(connection) {
  if (!connection) return ""
  if (connection.peer_reconnect) return "Trying to reconnect the Apple device..."
  var stage = connection.stage
  if (!stage || stage === "idle" || stage === "ready") return ""
  if (stage === "waiting") return "Waiting for the case to open or a bud to be removed."
  if (stage === "interrupted") return "Disconnected. Waiting for activity or Play."
  if (stage === "failed") {
    var reason = connection.reason ? " (" + connection.reason + ")" : ""
    return "AirPods connection unavailable" + reason + ". Waiting for desktop activity."
  }
  if (connection.recovering || stage === "retrying") {
    var attempt = connection.retry > 0 ? " (" + connection.retry + " of " + connection.retry_limit + ")" : ""
    return "Retrying connection" + attempt + "..."
  }
  return "Connecting to AirPods."
}

// Pods docked in the case cannot answer, so the daemon's control retries there are not an attempt to show.
function caseQuietConnection(connection, inCase) {
  if (!inCase || !connection || connection.peer_reconnect) return connection
  var retrying = connection.recovering || connection.stage === "retrying" || connection.stage === "interrupted"
  return retrying ? Object.assign({}, connection, { stage: "idle", recovering: false }) : connection
}

// An AirPods connection attempt still under way, with its retry once one has started; null otherwise.
function connectionAttempt(connection) {
  if (!connection || connection.peer_reconnect) return null
  var stage = connection.stage
  if (!stage || ["idle", "ready", "waiting", "failed", "interrupted"].indexOf(stage) >= 0) return null
  var retrying = connection.recovering || stage === "retrying"
  var count = connection.retry > 0 ? " " + connection.retry + " of " + connection.retry_limit : ""
  return { detail: retrying ? "(Retrying" + count + ")" : "" }
}

// The dots are padded to three so a retry beside them stays put in the monospace console.
function connectingLine(detail, dots, connected, device) {
  if (connected) return "Connecting to AirPods... Connected!"
  var marks = ".".repeat(dots)
  return "Connecting " + (device ? device + " " : "") + "to AirPods" + (detail ? (marks + "   ").slice(0, 3) + " " + detail : marks)
}

function connectionLog(connection, refreshing, caption, phase) {
  var progress = controlConnectionLabel(connection)
  var ready = connection && connection.stage === "ready" && !connection.peer_reconnect
  // Failures have their reason row; an interrupted link has its connectionHold status row.
  var hidden = connection && !connection.peer_reconnect && (connection.stage === "waiting" || connection.stage === "failed" || connection.stage === "interrupted")
  var connectionEvent = ready ? { kind: "connected" } : progress && !hidden ? { kind: "connection", connection: connection } : null
  var statusEvent = progress || refreshing || (ready && caption && caption.kind === "inCase") ? null : caption
  return {
    connection: statusText(connectionEvent), connectionEvent: connectionEvent,
    status: statusText(statusEvent), statusEvent: statusEvent,
    phase: progress ? "" : phase
  }
}

function handoffPhaseLabel(phase) {
  switch (phase) {
    case "scan": return "Connecting Bluetooth"
    case "rapport": return "Asking Apple to hand off"
    case "profile_connect": return "Connecting the audio profile"
    case "control": return "Opening the control link"
    case "control_audio": return "Waiting for the AirPods output"
    case "priority_prepare": return "Setting connection priority"
    case "priority_verify": return "Confirming connection priority"
    case "ownership": return "Claiming the Omarchy seat"
    case "seat_audio": return "Arming the AirPods output"
    case "await_audio": return "Waiting for playback"
    case "routing":
    case "audio": return "Routing audio to the buds"
    case "recovery_speakers": return "Moving audio to the speakers"
    case "recovery_suspend": return "Pausing the AirPods output"
    case "recovery_resume": return "Resuming the AirPods output"
    case "recovery_route": return "Routing audio again"
    case "rebuild_route": return "Rebuilding the audio route"
    case "recovered_audio": return "Checking playback"
    case "reconnect_audio": return "Reconnecting audio"
    default: return ""
  }
}

// The daemon resolves who holds the audio from the pods' reports: holder "local", "remote" or "" for unknown,
// and playing when a current source report decided it.
function audioOwner(status) {
  var owner = status.connected && status.audioOwner
  if (owner && owner.holder) return owner
  if (transportPlaybackConfirmed(status)) return { holder: "local", playing: true }
  return owner || { holder: "", playing: false }
}

// AACP can reconnect while A2DP is streaming. A player, selected sink, or
// Bluetooth ACL alone does not prove playback. Preserve a known other owner.
function transportPlaybackConfirmed(status) {
  if (status.transportPlaybackConfirmed === true) return true
  var owner = status.connected && status.audioOwner
  if (owner && owner.holder && !(owner.holder === "local" && owner.playing)) return false
  var audio = status.nativeHandoff
  return !!(status.bluezConnected === true && status.audioTransportActive === true
    && audio && audio.audio_active === true && audio.audio_output === "airpods"
    && audio.yielded !== true && !podsDocked(status))
}

// Docking silences playback without changing which connected device holds the seat.
function podsDocked(status) {
  var left = status.left || {}, right = status.right || {}
  return !status.isHeadset && !left.inEar && !right.inEar
    && (status.caseConfirmedDocked === true || (left.inCase === true && right.inCase === true))
}

function playingOn(status, holder) {
  var owner = audioOwner(status)
  return owner.playing && owner.holder === holder
}

function confirmedLocalPlayback(status) {
  var audio = status.nativeHandoff
  return !!(audio && audio.audio_active && audio.audio_output === "airpods" && playingOn(status, "local"))
}

function audioRouteState(previous, status) {
  previous = previous || { initialized: false, output: "", audible: "", message: "", event: null }
  // Off the pods the fleet broadcast names who is playing. It is a move only from another device this laptop knew
  // was playing; otherwise it is taken as the starting point, so joining beside that device later is not a move.
  var shared = status.connected ? null : fleetRosterView(status)
  var fleetSource = shared && !shared.pending && shared.sourcePlaying ? shared.sourceAddress : ""
  if (fleetSource && String(previous.audible || "").indexOf("remote:") !== 0)
    previous = Object.assign({}, previous, { initialized: true, audible: "remote:" + fleetSource, guessed: false })
  var native = status.nativeHandoff || {}
  var output = native.audio_output || previous.output
  var remote = !!fleetSource || playingOn(status, "remote")
  var address = fleetSource || normalizeAddress(status.remoteSourceAddress)
  // Setup can report remote playback before its address. That gap is not a change of owner.
  if (remote && !address && String(previous.audible || "").indexOf("remote:") === 0)
    address = previous.audible.slice(7)
  // The holder of the AirPods' audio: another device, or this laptop playing through them. Its own speakers or another
  // output hold nothing, so a reroute bounce through them leaves the holder as it was.
  var audible = remote ? "remote:" + address
    : native.audio_active === true && native.audio_output === "airpods"
      && (native.audio_state === "playing" || confirmedLocalPlayback(status)) ? "local:airpods" : ""
  // With nothing playing, the output is only a guess at the starting point; another device later seen playing was
  // already playing, so it replaces the guess quietly, while local playback is still a move this laptop made.
  if (!previous.initialized) return {
    initialized: !!(output || audible), output: output, audible: audible || (output ? "local:" + output : ""),
    guessed: !audible, message: "", event: null
  }
  var event = previous.event || null
  if (audible && previous.guessed && remote)
    return { initialized: true, output: output, audible: audible, guessed: false, message: statusText(event), event: event }
  if (audible && audible !== previous.audible) {
    event = null
    var local = normalizeAddress(status.localAddress)
    var device = local ? peerInfo(local, local, status.localName, status.devices).label : hostLabel(status.localName)
    if (remote) {
      var peer = status.devices && status.devices[address]
      event = { kind: "audioTaken", device: peer && peer.label ? peer.label : "Apple device" }
    } else if (output === "airpods") event = { kind: "audioTaken", device: device }
  }
  return { initialized: true, output: output, audible: audible || previous.audible, guessed: previous.guessed && !audible,
    message: statusText(event), event: event }
}

function audioFailure(reason) {
  var message = String(reason || "").trim()
  return ["ownership_changed", "playback_stopped", "playback_paused"].indexOf(message) >= 0 ? "" : message
}

function audioFailureMessage(reason, connected, device) {
  var message = audioFailure(reason)
  var detail = ""
  if (message === "profile_error:org.bluez.Error.InProgress") detail = "audio connection is still busy"
  else if (message.indexOf("profile_error:") === 0) detail = "audio connection failed"
  else if (message === "audio_profile_timeout") detail = "audio connection timed out"
  else if (message === "audio_route_timeout") detail = "audio output selection timed out"
  else if (message.indexOf("audio_operation_timeout:") === 0) detail = "audio routing timed out"
  else if (message.indexOf("audio_operation_failed:") === 0) detail = "audio routing failed"
  else if (message === "audio_timeout") detail = "audio playback did not start"
  if (!detail) return message
  return (connected ? (device || "AirPods") + " connected; " + detail
    : detail.charAt(0).toUpperCase() + detail.slice(1)) + "."
}

function idleAudioReady(status) {
  var audio = status.nativeHandoff
  return status.connected === true && status.controlConnection && status.controlConnection.stage === "ready"
    && audio && audio.audio_output === "airpods" && !audio.connecting && !audioFailure(audio.failure)
    && (status.reportedOwnership === -1 || status.reportedOwnership === 1)
    && status.sourceUpdated > 0 && status.audioSourceLocal === 1 && status.audioSourceType === 0
    && (status.left.inEar || status.right.inEar) ? true : false
}

// The label plus its typed kind: fleet labels name this laptop's fleet
// seat, the rest are plain audio states. Callers decide on the kind,
// never on the rendered words.
function localAudio(state, failure, worn, playbackConfirmed, device, idleOwnerConfirmed) {
  if (playbackConfirmed && !audioFailure(failure)) return { text: playingLabel(device, "Omarchy"), fleet: true }
  if (!state || state === "ready") return { text: fleetLabel("Omarchy", device), fleet: true }
  if (state === "playing") return { text: playingLabel(device, "Omarchy"), fleet: true }
  if (audioFailure(failure)) return { text: "Audio needs attention", fleet: false }
  // Idle ownership does not require an active audio output. Activity can
  // transfer that ownership while both laptops retain their control links.
  if (state === "unavailable" && idleOwnerConfirmed) return { text: fleetLabel("Omarchy", device), fleet: true }
  if (state === "unavailable" && worn === false) return { text: "Connected", fleet: false }
  switch (state) {
    case "routing": return { text: "Routing audio", fleet: false }
    case "starting": return { text: "Starting audio", fleet: false }
    case "profile_missing": return { text: "Preparing audio", fleet: false }
    case "output_elsewhere": return { text: "Another output selected", fleet: false }
    // Shown only while the pods name this laptop the owner, so a leftover release still reads as its fleet.
    case "remote": return { text: fleetLabel("Omarchy", device), fleet: true }
    default: return { text: "Audio unavailable", fleet: false }
  }
}

function localAudioLabel(state, failure, worn, playbackConfirmed, device, idleOwnerConfirmed) {
  return localAudio(state, failure, worn, playbackConfirmed, device, idleOwnerConfirmed).text
}

// The seats show who stays where, so a switch names only where the audio went.
// The AirPods started playing from another device: the event, named for where the audio went.
function audioSwitchCaption(device) {
  return "AirPods audio moved to " + device + "."
}

// Where the AirPods' audio currently is, whether or not anything is playing.
function audioHeldCaption(device) {
  return "AirPods audio is on " + device + "."
}

function domainEvent(linuxHeld, appleDevices, omarchyDevices, holdingOmarchySeat, disconnected, takingSeat, ownershipUnknown, bluezConnected) {
  var omarchy = (omarchyDevices && omarchyDevices[0] && omarchyDevices[0].label) || "this machine"
  if (takingSeat) return { kind: "claiming", device: omarchy }
  if (holdingOmarchySeat) return { kind: "reclaiming" }
  // A Bluetooth ACL alone does not confirm the seat's control connection.
  if (disconnected) return { kind: bluezConnected ? "controlPending" : "notConnected", device: omarchy }
  if (ownershipUnknown) return null
  if (linuxHeld) return { kind: "audioOn", device: omarchy }
  if (omarchyDevices && omarchyDevices.length) return { kind: "appleHasAudio", device: omarchy, seated: omarchyDevices[0].onAcl === true }
  return { kind: "appleHasAudio", device: "" }
}

function linuxHeld(status) {
  return audioOwner(status).holder === "local"
}

function remoteOmarchyAudio(status) {
  if (!playingOn(status, "remote")) return false
  var address = normalizeAddress(status.remoteSourceAddress)
  var peer = status.devices && status.devices[address]
  return !!peer && peer.kind === "omarchy"
}

// With nothing playing, the pods' roster still names the audio owner. Every laptop draws that Omarchy owner,
// this one included, so both panels agree even when the owner has stepped back to its speakers.
// A source report older than the latest ownership change predates a release, so it no longer outranks the roster.
function sourcePlaying(status) {
  return audioOwner(status).playing
}

function omarchyRosterOwner(status) {
  if (!status.connected || sourcePlaying(status)) return ""
  var owner = normalizeAddress(status.rosterAudioOwner)
  if (!owner) return ""
  if (owner === normalizeAddress(status.localAddress)) return owner
  var peer = status.devices && status.devices[owner]
  return peer && peer.kind === "omarchy" ? owner : ""
}

// A release only says this laptop stopped owning the audio. The pods hand it to another connected laptop
// when there is one, and the roster names it.
function appleOwnsAudio(status) {
  if (!status.connected || remoteOmarchyAudio(status)) return false
  var owner = audioOwner(status)
  if (owner.playing) return owner.holder !== "local"
  return owner.holder === "remote" && omarchyRosterOwner(status) === ""
}

// A paused Apple device still holding the audio is named by the roster too.
function appleRosterOwner(status) {
  if (!status.connected || sourcePlaying(status)) return ""
  var owner = normalizeAddress(status.rosterAudioOwner)
  if (!owner || owner === normalizeAddress(status.localAddress)) return ""
  var peer = status.devices && status.devices[owner]
  return peer && peer.kind === "apple" ? owner : ""
}

// The pods switch between two fleets, Apple's devices and Omarchy laptops; the member holding audio is the useful fact.
function fleetLabel(fleet, member) {
  return fleet + " fleet" + (member ? " · " + member : "")
}

function playingLabel(member, fleet) {
  return fleetLabel(fleet, member) + " · Playing"
}

function appleFleetSubtitle(status) {
  var member = appleFleetMember(status)
  return playingOn(status, "remote") ? playingLabel(member, "Apple") : fleetLabel("Apple", member)
}

// The Apple device holding audio: the roster names an idle owner, the source report a playing one.
function appleFleetMember(status) {
  var address = appleRosterOwner(status)
  if (!address && playingOn(status, "remote")) address = normalizeAddress(status.remoteSourceAddress)
  var peer = address && status.devices && status.devices[address]
  return peer && peer.kind === "apple" ? peer.label || "" : ""
}

function connectionPresentation(status, previous, nowMs) {
  var refreshWaitMs = 5000
  var memoryMaxAgeMs = 120000
  previous = previous || {}
  var remembered = previous.lastKnown || null
  if (remembered && nowMs - remembered.seenMs >= memoryMaxAgeMs) remembered = null
  var docked = status.caseConfirmedDocked || confirmedInCase(status.caseLidState, status.left, status.right)
  if (docked) remembered = null
  var directRoster = status.connected && status.peerListUpdated > 0
  var rosterUnknown = !directRoster
    && (previous.rosterUnknown === true || (!status.connected && !!previous.lastKnown))
  var shared = !docked && !directRoster ? fleetRosterView(status) : null
  var observingFleet = shared && !status.connected && !bluetoothSetupPending(status) && !joinReleased(status)
  if (shared) remembered = { fromFleet: true, unconfirmed: true, seenMs: nowMs,
    seats: shared.seats, linuxHeld: false, label: shared.sourceLabel || shared.reporterLabel }
  if (remembered && remembered.fromFleet && directRoster) remembered = null
  // The last definite audio owner stands in while nothing current is: a pending roster, a gap between rosters,
  // or a joined laptop's first moments before its own source report.
  var lastAudio = previous.lastAudio || null
  if (shared && !shared.pending)
    lastAudio = { appleOwnsAudio: shared.appleOwnsAudio, sourceAddress: shared.sourceAddress, playing: shared.sourcePlaying, seenMs: nowMs }
  var holder = audioOwner(status).holder
  var owner = holder === "local" ? 1 : holder === "remote" ? 0 : -1
  // A release to another connected laptop leaves that laptop, named by the roster, holding the audio.
  var rosterLaptop = owner === 0 && !appleOwnsAudio(status) && !remoteOmarchyAudio(status) ? omarchyRosterOwner(status) : ""
  var ownerAddress = owner === 1 ? normalizeAddress(status.localAddress)
    : rosterLaptop || normalizeAddress(status.remoteSourceAddress)
  if (owner === 0 || owner === 1)
    lastAudio = { appleOwnsAudio: owner === 0 && appleOwnsAudio(status), seenMs: nowMs, sourceAddress: ownerAddress,
      playing: audioOwner(status).playing }
  // Carry the old audio owner only while the new roster still supports it.
  if (owner === -1 && directRoster && lastAudio
      && !(status.connectedPeers || []).some(function (address) {
        return normalizeAddress(address) === lastAudio.sourceAddress
      })) lastAudio = null
  // A confirmed fleet view still takes precedence in the panel; this only fills in when nothing current is definite.
  var audio = owner === -1 && lastAudio && nowMs - lastAudio.seenMs < AUDIO_CARRY_MS ? lastAudio : null
  // Audio reports cannot replace a seat map before the direct roster arrives.
  if ((owner === 0 || owner === 1) && !docked && !(remembered && remembered.fromFleet)) {
    var seats = seatOccupants(status.connectedPeers, status.localAddress, status.localName,
      status.devices, status.connected, status.remoteSourceAddress)
    var remoteOmarchy = remoteOmarchyAudio(status)
    var omarchy = owner === 1 || remoteOmarchy || rosterLaptop !== ""
    var activeDevices = omarchy ? seats.omarchy : seats.apple
    remembered = {
      linuxHeld: omarchy, seenMs: nowMs,
      seats: seats,
      appleOwnsAudio: owner === 0 && !omarchy,
      sourceAddress: ownerAddress,
      playing: audioOwner(status).playing,
      label: remoteOmarchy || rosterLaptop ? peerInfo(rosterLaptop || status.remoteSourceAddress, status.localAddress, status.localName, status.devices).label
        : activeDevices.length ? activeDevices[0].label : omarchy ? "Omarchy" : "Apple device"
    }
  }
  var fleetMemoryMs = 30000
  if (remembered && remembered.fromFleet) {
    var control = status.controlConnection || {}
    var joining = status.connected || bluetoothSetupPending(status) || controlOpening(control)
      || (status.fleet && status.fleet.routing && status.fleet.routing.busy)
    var remaining = Math.max(0, fleetMemoryMs - (nowMs - remembered.seenMs))
    var provisional = !observingFleet && joining && remaining > 0 ? Object.assign({}, remembered) : null
    // The helper leaves once it lets go. This laptop is seated only when its own roster confirms it.
    var joinRouting = status.fleet && status.fleet.routing
    var released = status.connected || bluetoothSetupPending(status) || joinReleased(status)
      || (joinRouting && joinRouting.phase === "success")
    if (provisional && released) {
      var helper = joinRouting && joinRouting.peer ? normalizeAddress(joinRouting.peer) : ""
      // The admission names the device the helper keeps; a playing helper keeps itself and frees the other slot.
      var kept = joinRouting && joinRouting.retain ? normalizeAddress(joinRouting.retain) : ""
      var stays = function (device) {
        var address = normalizeAddress(device.address)
        return kept ? address === kept : address !== helper
      }
      provisional.seats = { apple: kept ? remembered.seats.apple.filter(stays) : remembered.seats.apple,
        omarchy: helper || kept ? remembered.seats.omarchy.filter(stays) : [] }
      provisional.localWaiting = true
    }
    return { waiting: !!provisional, startedMs: remembered.seenMs, remainingMs: provisional ? remaining : 0,
      refreshing: !!provisional, lastKnown: remaining > 0 ? remembered : null, snapshot: provisional,
      lastAudio: lastAudio, audio: audio, rosterUnknown: rosterUnknown }
  }
  // This laptop gave its slot to a fleet peer. Hold the last seats, minus this laptop, until the peer's roster lands.
  // The control socket closes before BlueZ reports the disconnect; neither event removes the retained Apple peer.
  var routing = status.fleet && status.fleet.routing
  // A saved success describes an earlier transaction. It can extend an
  // existing handover until its roster lands, but cannot start one on a later
  // unrelated loss of the local link (for example Mac replacing iPhone).
  var continuingHandover = routing && routing.phase === "success"
    && previous.snapshot && previous.snapshot.handover
  var handingOver = (routing && routing.busy) || continuingHandover
    || (status.controlConnection && status.controlConnection.peer_reconnect)
  if (remembered && !remembered.fromFleet && handingOver && !status.connected
      && nowMs - remembered.seenMs < fleetMemoryMs) {
    var handover = Object.assign({}, remembered, { handover: true, linuxHeld: false,
      peerLabel: routing && routing.peer ? peerInfo(routing.peer, status.localAddress, status.localName, status.devices).label : "", seats: {
      apple: remembered.seats.apple,
      omarchy: remembered.seats.omarchy.filter(function (device) {
        return normalizeAddress(device.address) !== normalizeAddress(status.localAddress)
      })
    } })
    return { waiting: false, startedMs: remembered.seenMs, remainingMs: fleetMemoryMs - (nowMs - remembered.seenMs),
      refreshing: true, lastKnown: remembered, snapshot: handover, lastAudio: lastAudio, audio: audio, rosterUnknown: rosterUnknown }
  }
  var waiting = status.connected && owner !== 0 && owner !== 1
    && !status.wantOmarchySeat && !status.holdingOmarchySeat && !docked
  var startedMs = waiting && previous.waiting ? previous.startedMs : nowMs
  var remainingMs = waiting ? Math.max(0, refreshWaitMs - (nowMs - startedMs)) : 0
  var snapshot = remainingMs > 0 ? remembered : null
  // A confirmed local link takes precedence over the remembered seat map.
  if (snapshot && (status.connected || status.bluezConnected)) {
    snapshot = Object.assign({}, snapshot, {
      seats: seatOccupants(status.connectedPeers, status.localAddress, status.localName,
        status.devices, status.connected || status.bluezConnected, status.remoteSourceAddress)
    })
  }
  return { waiting: waiting, startedMs: startedMs, remainingMs: remainingMs,
    refreshing: remainingMs > 0, lastKnown: remembered, snapshot: snapshot, lastAudio: lastAudio, audio: audio, rosterUnknown: rosterUnknown }
}

function elideError(text) {
  var value = String(text || "").replace(/\s+/g, " ").trim()
  return value.length > MAX_ERROR_CHARS ? value.substring(0, ELIDED_ERROR_CHARS) + "…" : value
}

function ownershipText(status) {
  if (!status.connected) return "Bluetooth disconnected"
  var holder = audioOwner(status).holder
  return "Bluetooth connected · " + (holder === "local" ? "Audio owned by Linux"
    : holder === "remote" ? "Audio owned by another device" : "Audio ownership unknown")
}

// A ready control link or observed local AirPods playback confirms this laptop's seat.
function localSeatConnected(status) {
  var control = status.controlConnection || {}
  return transportPlaybackConfirmed(status)
    || (status.connected === true && (!control.stage || control.stage === "ready"))
}

// Past admission the helper has let go, so its last roster broadcast is out of date. The helper keeps its seat only
// when it plays or when an idle phone can give up its slot instead, so it is drawn leaving from the request otherwise,
// including when its roster names no audio source.
function joinReleased(status) {
  var routing = status.fleet && status.fleet.routing
  if (!routing) return false
  if (routing.phase === "connecting" || routing.phase === "waiting_completion") return true
  var roster = status.fleet.roster
  var source = roster ? normalizeAddress(roster.audio_source) : ""
  var idleOwner = !!roster && roster.audio_idle === true && !!source
  return routing.phase === "waiting_admission" && !!routing.peer && !idleOwner && source !== normalizeAddress(routing.peer)
}

// Who holds the audio, from the first source present: the confirmed fleet broadcast, this laptop's handover snapshot,
// the owner carried through a switch, then this laptop's own reports. Every owner field comes from that one source.
function seatAudioOwner(status, fleet, previous, carried) {
  var docked = podsDocked(status)
  var from = function (source, apple, address, playing) {
    return { from: source, apple: !!apple, omarchy: !apple && !!address, address: address || "", playing: playing === true && !docked }
  }
  if (fleet && !fleet.pending) return from("fleet", fleet.appleOwnsAudio, fleet.sourceAddress, fleet.sourcePlaying)
  if (previous && previous.handover) return from("handover", previous.appleOwnsAudio, previous.sourceAddress, previous.playing)
  if (carried) return from("carried", carried.appleOwnsAudio, carried.sourceAddress, carried.playing)
  // The roster derivation here is deliberate, not a second-guessing of
  // the daemon: the daemon's holder is per-machine, while the roster is
  // the fleet-sync evidence that lets every laptop draw the same owner
  // (including a peer that has stepped back to its speakers). Giving
  // the daemon holder veto power breaks exactly those cases; the
  // recorded replay sessions pin them. Change this only with them.
  var omarchyOwner = omarchyRosterOwner(status), appleOwner = appleRosterOwner(status)
  var appleAudio = appleOwnsAudio(status)
  var omarchyAudio = !!(status.audioHere || status.remoteOmarchyAudio || omarchyOwner !== ""
    || (status.idleAudioReady && !appleAudio))
  var address = status.localPlaybackConfirmed ? status.localAddress
    : appleOwner !== "" ? appleOwner
    : status.remoteOmarchyAudio || appleAudio ? status.remoteSourceAddress
    : omarchyOwner !== "" ? omarchyOwner
    : omarchyAudio ? status.localAddress : ""
  // Remote streaming needs a named remote roster owner before it can light a device.
  var remoteRosterPlaying = status.remotePlaying === true && (appleOwner !== ""
    || (omarchyOwner !== "" && omarchyOwner !== normalizeAddress(status.localAddress)))
  return { from: "live", apple: appleOwner !== "" || appleAudio, omarchy: omarchyAudio, address: address || "",
    playing: !docked && (audioOwner(status).playing === true || remoteRosterPlaying) }
}

function fleetViewSubtitle(view) {
  var fleet = view.omarchyOwnsAudio ? "Omarchy" : view.appleOwnsAudio ? "Apple" : ""
  if (!fleet) return "Connected nearby"
  return view.sourcePlaying ? playingLabel(view.sourceLabel, fleet) : fleetLabel(fleet, view.sourceLabel)
}

// An ACL can outlive the control socket, including across daemon restarts.
// Only an actual setup stage makes that ACL evidence of a join in progress.
// Older daemons without a control stage retain their previous presentation.
function bluetoothSetupPending(status) {
  var control = status.controlConnection
  return status.bluezConnected === true && (!control || !control.stage || controlOpening(control))
}

function fleetSeatView(status) {
  return status.connected || bluetoothSetupPending(status) || joinReleased(status) ? null : fleetRosterView(status)
}

function fleetRosterView(status) {
  var roster = status.fleet && status.fleet.roster
  if (!roster || !roster.reporter || !roster.peers || !roster.peers.length) return null
  var peers = addressList(roster.peers)
  var local = normalizeAddress(status.localAddress)
  if (peers.length > 2 || peers.indexOf(normalizeAddress(roster.reporter)) < 0 || peers.indexOf(local) >= 0) return null
  var names = Object.assign({}, status.devices || {})
  if (local) names[local] = { label: hostLabel(status.localName), kind: "omarchy", nearby: true }
  var source = !roster.pending && peers.indexOf(normalizeAddress(roster.audio_source)) >= 0 ? normalizeAddress(roster.audio_source) : ""
  var sourcePeer = source ? peerInfo(source, local, status.localName, names) : null
  return {
    seats: seatOccupants(peers, "", "", names, false, source),
    availableApple: availableDevices("apple", peers, names, "", source),
    availableOmarchy: availableDevices("omarchy", peers, names, "", source),
    appleOwnsAudio: !!sourcePeer && sourcePeer.kind === "apple",
    omarchyOwnsAudio: !!sourcePeer && sourcePeer.kind === "omarchy",
    sourceAddress: source,
    sourceLabel: sourcePeer ? sourcePeer.label : "",
    // The connected laptop also broadcasts the pods' idle owner, flagged so it is not read as playback.
    sourcePlaying: !!sourcePeer && roster.audio_idle !== true && !podsDocked(status),
    reporterLabel: peerInfo(roster.reporter, local, status.localName, names).label,
    pending: roster.pending === true
  }
}

function seatPresentation(status, takingOmarchySeat, heldOmarchySeat) {
  var connection = status.connectionPresentation || {}
  var caseClosed = podsInClosedCase(status)
  var docked = podsDocked(status) || caseClosed
  var local = normalizeAddress(status.localAddress)
  var localConnected = status.daemonReachable && localSeatConnected(status)
  var fleet = status.daemonReachable ? fleetSeatView(status) : null
  var refreshing = !!(status.daemonReachable && !transportPlaybackConfirmed(status) && connection.refreshing
    && ((!takingOmarchySeat && !status.connecting) || (connection.snapshot && connection.snapshot.unconfirmed)
      || (status.controlConnection && status.controlConnection.peer_reconnect)))
  var previous = refreshing ? connection.snapshot : null
  var seatMap = seatOccupants(status.connectedPeers, status.localAddress, status.localName,
    status.devices, localConnected, status.remoteSourceAddress)
  if (!localConnected) seatMap.omarchy = seatMap.omarchy.filter(function (d) { return d.address !== local })
  // What is drawn: the fleet view, even pending, else the remembered snapshot, else this laptop's own roster.
  var seats = fleet ? fleet.seats : previous ? previous.seats : seatMap
  if (!localConnected)
    seats = { apple: seats.apple, omarchy: seats.omarchy.filter(function (d) { return d.address !== local }) }
  var localWaiting = !!previous && !!previous.localWaiting
  var omarchyOwner = status.daemonReachable ? omarchyRosterOwner(status) : ""
  var appleOwner = status.daemonReachable ? appleRosterOwner(status) : ""
  var ownerUnknown = !linuxHeld(status) && omarchyOwner === "" && appleOwner === ""
    && !status.remoteOmarchyAudio && !appleOwnsAudio(status) && !takingOmarchySeat
  var owner = status.daemonReachable ? seatAudioOwner(status, fleet, previous, connection.audio)
    : { from: "", apple: false, omarchy: false, address: "", playing: false }
  if (!localConnected && owner.address === local)
    owner = { from: "", apple: false, omarchy: false, address: "", playing: false }
  // A short gap in this laptop's control link does not remove the Apple device
  // whose audio we are still carrying. Keep its seat and name with that evidence;
  // a fresh roster or fleet view always takes precedence.
  var carriedApple = !fleet && !previous && owner.from === "carried" && owner.apple
    && !(status.connected && status.peerListUpdated > 0) && connection.lastKnown
    ? connection.lastKnown.seats.apple.find(function (device) {
        return normalizeAddress(device.address) === owner.address
      }) : null
  if (carriedApple && seats.apple.length === 0)
    seats = { apple: [Object.assign({}, carriedApple)], omarchy: seats.omarchy }
  // Losing this observer's link invalidates its Apple roster and audio owner.
  // BLE can report a connection elsewhere, but cannot identify its devices.
  var rememberedApple = connection.lastKnown && connection.lastKnown.seats.apple || []
  // The pods can confirm their peers before this laptop finishes setup. That
  // fresh roster already confirms the Apple seat, independently of our readiness.
  var directRoster = status.connected && status.peerListUpdated > 0
  var appleSeatUnknown = !!status.daemonReachable && !transportPlaybackConfirmed(status) && !directRoster
    && (!localConnected || connection.rosterUnknown) && !fleet
    && !(previous && (previous.handover || previous.fromFleet))
    && !docked && (seats.apple.length > 0 || rememberedApple.length > 0
      || Object.keys(status.devices || {}).some(function (address) { return status.devices[address].kind === "apple" }))
  if (appleSeatUnknown) {
    seats = { apple: [], omarchy: seats.omarchy }
    owner = { from: "", apple: false, omarchy: false, address: "", playing: false }
    carriedApple = null
  }
  var peer = carriedApple || (owner.address ? peerInfo(owner.address, status.localAddress, status.localName, status.devices)
    : { label: "", kind: "" })
  var seated = seats.apple.concat(seats.omarchy).map(function (device) { return device.address })
  var routing = status.fleet && status.fleet.routing
  var givingSeat = !!(routing && routing.busy && routing.peer
    && (routing.phase === "admitting" || routing.phase === "waiting_connection")
    && normalizeAddress(routing.retain) !== local)
  var switching = !!status.daemonReachable && !(fleet && !fleet.pending) && (givingSeat || !!(fleet && fleet.pending)
    || !!(previous && (previous.handover || previous.unconfirmed))
    || (!takingOmarchySeat && backgroundJoin(status)))
  var replacingOmarchy = givingSeat || !!(previous && previous.handover)
    || !!(previous && previous.unconfirmed && connection.lastKnown
      && connection.lastKnown.seats && connection.lastKnown.seats.omarchy.some(function (device) {
        return normalizeAddress(device.address) !== normalizeAddress(status.localAddress)
      }))
    || !!(fleet && fleet.pending && fleet.seats.omarchy.length)
  // Another laptop gives up the seat, or this one asks a seated laptop for it. Either is drawn the same way, whichever
  // laptop is taking the seat: "Switching…" and no connector trace.
  var takingOver = (switching && replacingOmarchy) || routeRequesting(routing)
  var names = Object.assign({}, status.devices || {})
  if (appleSeatUnknown)
    rememberedApple.forEach(function (device) {
      if (!names[device.address]) names[device.address] = { label: device.label, kind: "apple" }
    })
  if (local) names[local] = { label: hostLabel(status.localName), kind: "omarchy", nearby: true }
  var waiting = fleet ? fleet.availableOmarchy.slice() : availableDevices("omarchy", seated,
    names, local, status.remoteSourceAddress, localConnected && !localWaiting)
  if (!localConnected && local && !waiting.some(function (d) { return d.address === local }))
    waiting.push(peerInfo(local, local, status.localName, names))
  var control = status.controlConnection || {}
  var localAttempt = takingOmarchySeat || controlOpening(control) || routeRequesting(status.fleet && status.fleet.routing)
  // The one connection attempt, as a typed kind; the transition label is
  // rendered from it and never read back as state.
  var attemptKind = caseClosed ? "none"
    : control.peer_reconnect ? "waiting"
    : !localConnected && seats.omarchy.length === 0
    ? (takingOver ? "switching"
      : localAttempt ? (control.recovering ? "reconnecting" : "connecting") : "none")
    : switching ? (replacingOmarchy ? "switching" : "connecting")
    : "none"
  var attemptLabel = { none: "", waiting: "Waiting…", switching: "Switching…",
    connecting: "Connecting…", reconnecting: "Reconnecting…" }[attemptKind]
  return {
    fleetView: fleet, previousConnection: previous, refreshingConnection: refreshing,
    seatMap: seatMap, seats: seats, localWaiting: localWaiting, ownershipUnknown: appleSeatUnknown || ownerUnknown,
    omarchyRosterOwner: omarchyOwner, appleRosterOwner: appleOwner,
    appleOwnsAudio: owner.apple, omarchyOwnsAudio: owner.omarchy,
    audioDeviceAddress: owner.address, audioPeer: peer,
    audioSubtitle: fleet && !fleet.pending ? fleetViewSubtitle(fleet)
      : seatAudioSubtitle(owner.omarchy, owner.apple, peer.label, peer.kind, owner.playing),
    switching: switching,
    takingOverOmarchy: takingOver,
    attempt: { kind: attemptKind, label: attemptLabel },
    omarchyTransitionLabel: attemptLabel,
    appleSeatUnknown: appleSeatUnknown,
    appleTrayRemembered: appleSeatUnknown || (!localConnected && !fleet && docked),
    availableApple: fleet ? fleet.availableApple : availableDevices("apple", seated,
      names, status.localAddress, status.remoteSourceAddress),
    appleTrayAvailable: appleTrayAvailable(seats.apple, seats.omarchy, owner.apple,
      fleet ? fleet.availableApple : availableDevices("apple", seated,
        names, status.localAddress, status.remoteSourceAddress), localConnected,
      owner.apple && owner.playing),
    availableOmarchy: waiting,
    linuxHeld: !!(fleet ? fleet.omarchyOwnsAudio : previous ? previous.linuxHeld && seats.omarchy.length > 0
      : (localConnected && heldOmarchySeat) || status.remoteOmarchyAudio),
    viewOwnershipUnknown: appleSeatUnknown || (fleet ? !fleet.omarchyOwnsAudio && !fleet.appleOwnsAudio
      : previous ? !!previous.unconfirmed : ownerUnknown),
    connectedToOmarchy: !!localConnected && !localWaiting,
    audioPlaying: owner.playing
  }
}

// Steady states are drawn by the subtitle and the seats; this line reports only what this laptop is doing.
function localConnectionLabel(status) {
  if (transportPlaybackConfirmed(status)) return ""
  var control = status.controlConnection || {}
  if (control.stage === "interrupted") return "Disconnected · waiting for activity or Play"
  if (status.connected || bluetoothSetupPending(status))
    return control.stage && control.stage !== "ready" ? "This laptop is waiting for the AirPods to confirm" : ""
  // A closed case holds no link, so a reconnect there says nothing useful.
  if (status.caseConfirmedDocked || confirmedInCase(status.caseLidState, status.left, status.right)) return "AirPods in case"
  if (control.recovering || control.stage === "retrying") return "This laptop is reconnecting"
  if (controlOpening(control)) return "This laptop is connecting"
  if (control.stage === "failed") return "This laptop could not connect"
  return ""
}

// Seat layout: who sits in each seat, who waits in its tray and which device is lit. SeatsView draws and glides it.
function isPhone(label) {
  return String(label || "").toLowerCase().indexOf("phone") >= 0
}

// Source addresses survive reconnects, so a seat falls back to its only device when the source is not seated.
function seatAudioAddress(devices, activeAddress) {
  var address = normalizeAddress(activeAddress)
  if (devices.some(function (device) { return normalizeAddress(device.address) === address })) return address
  return devices.length === 1 ? normalizeAddress(devices[0].address) : ""
}

// The Apple tray is a connection affordance: another Apple device can join
// through the current Apple owner. Saved names alone do not enable it.
// Once this laptop holds the pods and the Apple seat is empty, the known
// Apple devices wait in the tray too: they may reconnect from there.
function appleTrayAvailable(appleDevices, omarchyDevices, appleOwns, availableApple, localConnected, seatedApplePlaying) {
  if (appleOwns && (appleDevices || []).some(function (device) { return device.onAcl === true })) return true
  // A seated Apple device that is not playing can be displaced by the
  // waiting one over Apple smart routing, whoever holds the audio: the
  // tray shows so that device can come forward.
  if (!seatedApplePlaying && (availableApple || []).length > 0
    && (appleDevices || []).some(function (device) { return device.onAcl === true })) return true
  return localConnected === true
    && !(appleDevices || []).some(function (device) { return device.onAcl === true })
    && (omarchyDevices || []).some(function (device) { return device.onAcl === true })
    && (availableApple || []).length > 0
}

function appleSeat(appleDevices, availableApple, activeAddress, reconnect, trayAvailable, remembered) {
  var devices = appleDevices.filter(function (device) { return device.onAcl === true })
  var waiting = trayAvailable === false && !remembered ? []
    : appleDevices.filter(function (device) { return device.onAcl !== true }).concat(availableApple)
  return {
    devices: devices, waiting: waiting, showWaiting: waiting.length > 0, linkConnected: devices.length > 0,
    audioDeviceAddress: seatAudioAddress(devices, activeAddress),
    // Reconnect works through this laptop's link; without it the phone can join the pods on its own.
    // An empty seat still offers it. The phone does not have to be sitting in the tray first.
    actionLabel: remembered ? "" : reconnect.busy ? (!devices.length ? "Reconnecting…" : "")
      : !devices.length && reconnect.connected ? "Reconnect" : "",
    actionEnabled: !remembered && reconnect.connected && reconnect.enabled && !reconnect.busy
  }
}

function omarchySeat(omarchyDevices, availableOmarchy, activeAddress, connected, linuxHeld, ownershipUnknown, localAddress) {
  // The daemon lists the pods' devices in join order on every laptop: the
  // oldest takes the left slot and the newest the right, wherever this panel runs.
  var devices = omarchyDevices.slice()
  return {
    devices: devices, waiting: availableOmarchy,
    showWaiting: availableOmarchy.length > 0 || (!ownershipUnknown && linuxHeld),
    linkConnected: connected || omarchyDevices.some(function (device) { return device.onAcl === true }),
    audioDeviceAddress: normalizeAddress(activeAddress), actionLabel: "", actionEnabled: true
  }
}

// Reconnect drops both laptops one after the other. The seat keeps the pair it held, so the one remaining neither
// recenters nor changes slot before it leaves too. One rejoining after the seat emptied is alone and centers.
function seatPairHold(hold, devices, reconnectBusy) {
  var pair = hold.pair
  if (devices.length === 2) pair = devices.slice()
  else if (!reconnectBusy || hold.emptied) pair = []
  return { pair: pair, emptied: pair.length === 2 && !devices.length }
}

// A laptop that has left keeps its slot as an unseen stand-in.
function seatedDevices(devices, hold, reconnectBusy) {
  var pair = hold.pair
  var inPair = function (device) {
    return pair.some(function (d) { return normalizeAddress(d.address) === normalizeAddress(device.address) })
  }
  if (!(reconnectBusy && pair.length === 2 && (!devices.length || (!hold.emptied && devices.length === 1 && inPair(devices[0])))))
    return devices
  return pair.map(function (d) {
    var present = devices.length === 1 && normalizeAddress(devices[0].address) === normalizeAddress(d.address)
    return present ? devices[0] : Object.assign({}, d, { onAcl: false, standIn: true })
  })
}

// The tray has two slots, ordered by when each device left a seat; the latest departure always shows.
function trayDevices(waiting, departedAt) {
  var departure = function (device) { return departedAt[normalizeAddress(device.address || "")] || 0 }
  var ordered = waiting.slice().sort(function (a, b) { return departure(a) - departure(b) })
  return ordered.length > 2 ? [ordered[0], ordered[ordered.length - 1]] : ordered
}

function seatDeviceLit(device, seat, ownsAudio) {
  if (!device || device.onAcl === false || !ownsAudio) return false
  return seat.audioDeviceAddress !== "" ? normalizeAddress(device.address) === seat.audioDeviceAddress
    : seat.devices.length === 1
}

// The status log and the panel's status view: the console's rows, the subtitle and the seat claim, from the daemon's
// status lines, the panel's seat clicks, sent commands and their refusals, and the clock. A recorded session replays
// through it (tests/replay.test.js). Each step takes an event { t, ev } with ev one of status (with status), gone,
// click, send (verb), reply (error) or tick, and returns the next state: rows, description, subtitle, showSeats,
// taking, linuxHeld, view (the status as the panel reads it), and wakeAt, when a held line or debounce next
// expires, so the caller steps it then.
var LOG_CAPACITY = 200

function statusLogState() {
  var s = Object.assign({
    rows: [], nextId: 1, previous: {}, errors: [], started: false,
    caption: null, description: "", subtitle: "", showSeats: false, guidanceVisible: true, taking: false,
    seat: seatPresentation(Object.assign(defaultStatus(), { connectionPresentation: { refreshing: false, snapshot: null } }), false, false),
    daemonReachable: false, statusLoaded: false, lastError: "", action: "", actionUntil: 0, handedOver: null, switchFailed: null,
    disconnectedMs: 0, claimHold: false, claimSeen: false, linuxHeld: false, leaveAt: 0, seen: {}
  }, statusLogDaemonState())
  s.view = statusLogPods(s)
  return s
}

// What the log learned from the daemon, cleared when its file is gone or unreadable.
function statusLogDaemonState() {
  return {
    status: defaultStatus(), fleet: { phase: "idle" }, presentation: { refreshing: false, snapshot: null }, presentationUntil: 0,
    audioRoute: {}, joined: null, joinSuccess: null, joinedUntil: 0, pendingIntent: null, pendingUntil: 0, ownershipIntent: "none",
    wantOmarchySeat: false, nativeHandoff: false, connecting: false, attempting: false, handoffPhase: "",
    connectionUpdatedMs: 0, connected: false, seatHold: false, seatHoldUntil: 0, lastInEarMs: -1,
    iconLidState: LID_UNKNOWN, localSeatHeld: false, connectionFailed: false, caseRecoveryQuiet: false, audioRecoveryPending: false,
    busyPeer: "", waitingForPeer: null, routeEnded: null, seatHandoff: false,
    handedOver: null, handedOverAddress: "", handedOverUntil: 0
  }
}

function statusLog(previous, event) {
  var s = Object.assign({}, previous || statusLogState())
  s.rows = s.rows.map(function (row) { return Object.assign({}, row) })
  var now = event.t
  s.now = now
  s.routeEnded = null
  if (s.handedOverUntil && now >= s.handedOverUntil) {
    s.handedOver = null; s.handedOverAddress = ""; s.handedOverUntil = 0
  }
  // Timers the service and panel run, settled at the moment of this event.
  if (s.joinedUntil && now >= s.joinedUntil) { s.joined = null; s.joinedUntil = 0 }
  if (s.actionUntil && now >= s.actionUntil) { s.action = ""; s.actionUntil = 0 }
  if (s.pendingUntil && now >= s.pendingUntil) { s.pendingIntent = null; s.pendingUntil = 0 }
  if (s.seatHoldUntil && now >= s.seatHoldUntil) { if (!s.connected) s.seatHold = false; s.seatHoldUntil = 0 }
  if (s.presentationUntil && now >= s.presentationUntil) {
    s.presentation = Object.assign({}, s.presentation, { refreshing: false, snapshot: null, remainingMs: 0 })
    s.presentationUntil = 0
  }
  if (s.presentation.audio && now >= s.presentation.audio.seenMs + AUDIO_CARRY_MS)
    s.presentation = Object.assign({}, s.presentation, { audio: null })
  if ((s.connecting || s.attempting) && now - s.connectionUpdatedMs >= 60000) s.connecting = s.attempting = false

  if (event.ev === "status") statusLogApply(s, event.status, now)
  else if (event.ev === "gone") {
    Object.assign(s, statusLogDaemonState())
    s.statusLoaded = true; s.daemonReachable = false; s.lastError = ""; s.started = true
  } else if (event.ev === "click") { s.claimHold = true; s.claimSeen = false }
  else if (event.ev === "send") {
    var optimistic = event.verb === "connect" ? "claim" : event.verb === "handoff:release" ? "release" : null
    if (optimistic) { s.pendingIntent = optimistic; s.ownershipIntent = optimistic }
    // A claim asks for the seat at once; the next status says whether the daemon kept it.
    if (event.verb === "connect") s.wantOmarchySeat = true
    s.pendingUntil = now + 4000
  } else if (event.ev === "reply") {
    s.pendingIntent = null; s.pendingUntil = 0
    s.action = event.error; s.actionUntil = now + 2200
  }
  if (s.started) {
    statusLogPanel(s, now)
    statusLogRecord(s, statusLogInputs(s, now))
  }
  s.view = statusLogPods(s)
  var wake = [s.joinedUntil, s.actionUntil, s.pendingUntil, s.seatHoldUntil, s.presentationUntil, s.leaveAt, s.handedOverUntil,
      s.presentation.audio ? s.presentation.audio.seenMs + AUDIO_CARRY_MS : 0]
    .filter(function (at) { return at > 0 })
  s.wakeAt = wake.length ? Math.min.apply(null, wake) : 0
  return s
}

function statusLogApply(s, status, now) {
  var pairJoined = observedFleetJoin(s.status, status)
  var ended = fleetOutcome(s.status, status)
  s.statusLoaded = true
  s.daemonReachable = true
  if (!status.ok) { Object.assign(s, statusLogDaemonState()); s.lastError = status.lastError; s.started = true; return }
  s.lastError = ""
  s.started = true
  var presentation = connectionPresentation(status, s.presentation, now)
  s.localSeatHeld = localSeatConnected(status)
  var nextFleet = status.fleet || { phase: "idle" }
  var route = nextFleet.routing
  // Even an alongside admission can displace us if the pods retain the Apple device instead.
  var giving = route && route.busy && (route.phase === "admitting" || route.phase === "waiting_connection")
  if (routeRequesting(route) || (ended && ended.result === "admit_failed")
      || (giving && s.handedOverAddress && normalizeAddress(route.peer) !== s.handedOverAddress)) {
    s.handedOver = null; s.handedOverAddress = ""; s.handedOverUntil = 0
  }
  if (giving || (ended && (ended.result === "admitted" || ended.result === "admitted_alongside"))) s.seatHandoff = true
  else if (localSeatConnected(status) || connectionAttempt(status.controlConnection) || routeRequesting(route)
    || (ended && ended.result === "admit_failed")) s.seatHandoff = false
  var retainedPeer = route && route.peer && normalizeAddress(route.retain) === normalizeAddress(route.peer)
  var localPair = pairJoined && (!(s.status.connected && s.status.controlConnection && s.status.controlConnection.stage === "ready")
    || (retainedPeer && routeAdmitted(route)))
  var label = function (address) { return peerInfo(address, status.localAddress, status.localName, status.devices).label }
  var joined = !!ended && (ended.result === "joined" || ended.result === "joined_alongside")
  if ((joined && ended.result === "joined_alongside") || localPair)
    s.joinSuccess = { kind: "joinedAlongside", peer: label(localPair ? connectedFleetPeer(status) : ended.peer) }
  if (joined) { s.joined = s.joinSuccess || { kind: "fleetSwitched", device: hostLabel(status.localName) }; s.joinedUntil = now + 3000 }
  // Completion can precede the peer's roster. Hold its log until the same
  // confirmed seat update that starts the incoming device's animation.
  // A failed request leaves the attempt to finish its retries in the freed slot; the log reports it once that attempt ends.
  // Without a control link it falls back to a direct connect instead, which logs its own attempt.
  if (status.connected || routeAdmitted(nextFleet.routing)) s.busyPeer = ""
  if (ended && ended.result === "peer_busy") {
    if (s.busyPeer !== ended.peer) s.waitingForPeer = { kind: "peerBusy", peer: label(ended.peer) }
    s.busyPeer = ended.peer
  } else if (ended && ended.result === "join_failed") {
    s.busyPeer = ""
    s.switchFailed = { kind: "switchFailed", device: hostLabel(status.localName) }
  }
  if (ended && ended.peer && (ended.result === "admitted" || ended.result === "admitted_alongside")) {
    s.handedOver = ended.result === "admitted_alongside"
      ? { kind: "pairedAlongside", device: label(ended.peer), peer: hostLabel(status.localName) }
      : { kind: "fleetSwitched", device: label(ended.peer) }
    s.handedOverAddress = normalizeAddress(ended.peer)
    s.handedOverUntil = now + 30000
  }
  s.routeEnded = ended
  if (pairJoined) {
    s.handedOver = localPair ? null : pairJoined
    s.handedOverAddress = ""; s.handedOverUntil = 0
    s.joined = s.joinSuccess || pairJoined
    s.joinedUntil = now + 3000
  }
  s.fleet = nextFleet
  s.audioRoute = audioRouteState(s.audioRoute, status)
  var advertisementMs = Number(status.lastAdvertisementMs || 0)
  if (status.left.inEar || status.right.inEar) s.lastInEarMs = advertisementMs
  s.iconLidState = status.lidUpdatedMs <= s.lastInEarMs ? LID_UNKNOWN : status.caseLidState
  if (s.nativeHandoff && !status.nativeHandoff) { s.connecting = s.attempting = false; s.handoffPhase = ""; s.connectionUpdatedMs = 0 }
  s.nativeHandoff = !!status.nativeHandoff
  if (s.nativeHandoff) s.connectionUpdatedMs = Number(status.nativeHandoff.updated_ms || 0)
  if (status.connected !== s.connected) {
    if (status.connected) s.seatHold = true
    else { s.disconnectedMs = now; s.seatHoldUntil = now + 3000 }
  }
  s.connected = status.connected
  s.wantOmarchySeat = status.wantOmarchySeat === true
  if (s.nativeHandoff) {
    s.attempting = status.nativeHandoff.attempting === true
    s.connecting = status.nativeHandoff.connecting === true
    s.handoffPhase = status.nativeHandoff.phase || ""
  }
  if (s.pendingIntent === null) s.ownershipIntent = status.ownershipIntent
  else if (status.ownershipIntent === s.pendingIntent) { s.ownershipIntent = status.ownershipIntent; s.pendingIntent = null; s.pendingUntil = 0 }
  else s.ownershipIntent = s.pendingIntent
  s.status = status
  s.presentation = presentation
  s.presentationUntil = presentation.refreshing ? now + presentation.remainingMs : 0
}

// The status as the panel reads it: the status plus the fields the log keeps across lines.
function statusLogPods(s) {
  var st = s.status
  var native = st.nativeHandoff
  var pods = Object.assign({}, st, {
    daemonReachable: s.daemonReachable, statusLoaded: s.statusLoaded, lastError: s.lastError,
    connectionPresentation: s.presentation, localSeatHeld: s.localSeatHeld, fleet: s.fleet,
    audioRoute: s.audioRoute, idleAudioReady: idleAudioReady(st), localPlaybackConfirmed: confirmedLocalPlayback(st),
    transportPlaybackConfirmed: transportPlaybackConfirmed(st),
    localAudioState: native ? native.audio_state || "" : "", localAudioFailure: native ? native.failure || "" : "",
    nativeHandoff: s.nativeHandoff,
    // The ring and the drawing line follow a fleet ask and a socket that is opening, not only a handoff.
    connecting: !transportPlaybackConfirmed(st) && (s.connecting
      || openingControlLink(s.connected === true, st.controlConnection, s.fleet && s.fleet.routing)),
    connectionAttempting: s.attempting, handoffPhase: s.handoffPhase,
    // Completed and connected before connecting goes false, so the ring can fill and fade instead of hiding.
    connectionCompleted: !!native && native.completed === true,
    connected: s.connected, ownershipIntent: s.ownershipIntent, wantOmarchySeat: s.wantOmarchySeat,
    bluezConnected: st.bluezConnected === true, holdingOmarchySeat: !transportPlaybackConfirmed(st) && st.holdingOmarchySeat === true,
    connectedPeers: st.connectedPeers || [], localAddress: st.localAddress || "", localName: st.localName || "",
    lastAdvertisementMs: Number(st.lastAdvertisementMs || 0), remoteSourceAddress: st.remoteSourceAddress || "",
    rosterAudioOwner: st.rosterAudioOwner || "", devices: st.devices || {},
    leftPod: st.left, rightPod: st.right, headsetBattery: st.headset, iconLidState: s.iconLidState,
    seatHold: s.seatHold, disconnectedMs: s.disconnectedMs
  })
  pods.audioHere = pods.localPlaybackConfirmed || (pods.ownershipIntent !== "release" && linuxHeld(pods))
  pods.remoteOmarchyAudio = remoteOmarchyAudio(pods)
  pods.hasAirPods = pods.daemonReachable && pods.connected
  // Battery keeps arriving over BLE while the audio link is down, so it is not gated on connected.
  pods.hasBattery = pods.daemonReachable && (pods.isHeadset ? pods.headset.level !== LEVEL_UNKNOWN
    : pods.left.level !== LEVEL_UNKNOWN || pods.right.level !== LEVEL_UNKNOWN || pods.caseBattery.level !== LEVEL_UNKNOWN)
  return pods
}

// The panel's claim hold and its 3 s debounce of the Omarchy owner, as its change handlers ran them. The hold keeps an
// early "still Apple" report from pulling the buds back mid-click; the debounce rides out ownership flaps after CLAIM.
function statusLogPanel(s, now) {
  var pods = statusLogPods(s)
  var live = linuxHeld(pods)
  var seen = s.seen
  var changed = function (name, value) { var was = seen[name]; seen[name] = value; return was !== undefined && was !== value }
  var settle = function () {
    if (live || !pods.wantOmarchySeat) { s.claimHold = false; s.claimSeen = false; return }
    if (s.claimSeen && !pods.connecting && !pods.holdingOmarchySeat && appleOwnsAudio(pods)) { s.claimHold = false; s.claimSeen = false }
  }
  s.seen = seen = Object.assign({}, seen)
  if (changed("connecting", pods.connecting) | changed("holding", pods.holdingOmarchySeat)) {
    if (pods.connecting || pods.holdingOmarchySeat) s.claimSeen = true
    settle()
  }
  if (changed("want", pods.wantOmarchySeat) && !pods.wantOmarchySeat) settle()
  if (changed("live", live)) settle()
  // A seated laptop may retain playback intent while another fleet member
  // owns audio. That intent is not an unfinished connection attempt.
  var taking = !live && (s.claimHold || (!pods.connected && pods.wantOmarchySeat
    && pods.ownershipIntent === "claim" && !appleOwnsAudio(pods)))
  var heldNow = taking || (pods.connected && live)
  if (seen.heldNow === undefined) s.linuxHeld = heldNow
  else if (heldNow) { s.linuxHeld = true; s.leaveAt = 0 }
  else if (s.linuxHeld && !s.leaveAt) s.leaveAt = now + 3000
  if (s.leaveAt && now >= s.leaveAt) { if (!heldNow) s.linuxHeld = false; s.leaveAt = 0 }
  seen.heldNow = heldNow
  s.taking = taking
}

// Device names a log line may mention: every known device and this laptop.
function logDeviceNames(devices, localName) {
  var names = [hostLabel(localName)]
  for (var key in devices || {}) {
    var label = String((devices[key] || {}).label || "").trim()
    if (label && names.indexOf(label) < 0) names.push(label)
  }
  return names
}

function escapeMarkup(text) {
  return String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;")
}

// A log line as styled text with each whole device name in its own colour, so names stand apart from the words around
// them. The longest name wins where two overlap, and a name inside a longer word is left alone.
function deviceNameMarkup(text, names, color) {
  var safe = escapeMarkup(text)
  var pattern = (names || []).map(function (name) { return escapeMarkup(String(name || "").trim()) })
    .filter(function (name, i, all) { return name && all.indexOf(name) === i })
    .sort(function (a, b) { return b.length - a.length })
    .map(function (name) { return name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") })
  if (!pattern.length) return safe
  var names = new RegExp("(^|[^A-Za-z0-9_-])(" + pattern.join("|") + ")(?![A-Za-z0-9_-])", "g")
  return safe.replace(names, function (match, before, name) {
    return before + '<font color="' + color + '">' + name + "</font>"
  })
}

// The words for a status event: a log row or the panel's description. Decisions compare kinds, never these words.
function statusText(event) {
  if (!event) return ""
  switch (event.kind) {
    case "offline": return "FleetPods is not running"
    case "waitingForPlayback": return event.device + " disconnected. Play audio or click to reconnect."
    case "joinedAlongside": return "Connected to AirPods alongside " + event.peer + "."
    case "pairedAlongside": return event.device + " connected alongside " + event.peer + "."
    case "fleetSwitching": return "Switching Omarchy Fleet connection to " + event.device + "…"
    case "fleetSwitched": return "Omarchy Fleet connection switched to " + event.device + "."
      + (event.keeper != null ? " " + event.keeper + " keeps the audio." : "")
    case "reporterConnected": return "AirPods connected to " + event.device + "."
    case "fleetUnconfirmed": return "Omarchy fleet switching. Last reported: " + event.device + "."
    case "connection": return controlConnectionLabel(event.connection)
    case "inCase": return "Available, in case"
    case "caseClosed": return "Case closed"
    case "elsewhere": return event.device
      ? event.device + " is connected. The AirPods are available."
      : "Connected elsewhere. The AirPods are available."
    case "unavailable": return event.headset ? "Unavailable" : "Unavailable · possibly in case"
    case "audioTaken": return audioSwitchCaption(event.device)
    case "audioOn": return audioHeldCaption(event.device)
    case "claiming": return event.device + " claims ownership."
    case "reclaiming": return "Reclaiming Omarchy seat. Apple keeps audio."
    case "connectedHere": return "Connected to " + event.device + "."
    case "controlPending": return "Bluetooth connected. Waiting for " + event.device + " to finish AirPods setup."
    case "notConnected": return event.device + " is not connected to the AirPods."
    case "appleHasAudio": return "Apple fleet has the audio. " + (!event.device ? "No Omarchy seat connection is confirmed."
      : event.device + (event.seated ? " keeps the Omarchy seat." : " is not confirmed in the Omarchy seat."))
    case "waitingDisconnect": return "Waiting for " + event.peer + " to disconnect..."
    case "connecting": return connectingLine(event.detail, 3, false, event.device)
    case "connected": return event.fromAttempt ? connectingLine("", 3, true) : "AirPods connected."
    case "connectingAgain": return "Trying to connect again..."
    case "peerBusy": return "Waiting for " + event.peer + "..."
    case "retryScheduled": return "Waiting for " + event.peer + "."
    case "retriesExhausted": return "Waiting for " + event.peer + ". Automatic retries stopped."
    case "retryCancelled": return "AirPods connection retry cancelled."
    case "retryRequested": return "Requested an AirPods connection via " + event.peer + "."
    case "retryStopped": return "Connection via " + event.peer + " stopped: " + event.reason
    case "switchFailed": return event.device + " couldn't connect to the AirPods."
    case "audioRestored": return event.device + " audio connection restored."
  }
  throw new Error("statusText: unknown status event kind " + event.kind)
}

function subtitleText(event) {
  switch (event.kind) {
    case "pairingRequired": return "Pair AirPods to get started"
    case "offline": return "Daemon offline"
    case "audio": return event.text
    case "waiting": return "Waiting for AirPods"
    case "connectionUnavailable": return "Connection unavailable"
    case "retrying": return "Retrying connection"
    case "connecting": return "Connecting"
    case "reconnecting": return "Reconnecting"
    case "refreshing": return "Refreshing connection"
    case "inCase": return "Available, in case"
    case "caseClosed": return "Case closed"
    case "elsewhere": return "Connected elsewhere"
    case "unavailable": return event.headset ? "Unavailable" : "Unavailable · possibly in case"
    case "notConnected": return "Not connected"
    case "connected": return "Connected"
  }
  throw new Error("subtitleText: unknown subtitle kind " + event.kind)
}

// What the panel hands the console: its caption, subtitle and the service's audio, phase and error lines.
function statusLogInputs(s, now) {
  var pods = statusLogPods(s)
  var taking = s.taking
  var seat = seatPresentation(pods, taking, s.linuxHeld)
  var fleetView = seat.fleetView, previous = seat.previousConnection
  var inCase = pods.caseConfirmedDocked || confirmedInCase(pods.iconLidState, pods.leftPod, pods.rightPod)
  var caseClosed = podsInClosedCase(pods)
  var control = pods.transportPlaybackConfirmed ? null
    : caseQuietConnection(pods.controlConnection, inCase && pods.iconLidState !== LID_OPEN)
  // A link lost during admission belongs to the fleet switch.
  if (s.seatHandoff && control && control.stage === "interrupted") control = null
  var controlDescription = controlConnectionLabel(control)
  var telemetry = telemetryUnavailable(pods.connected, pods.lastAdvertisementMs, now)
  var elsewhere = pods.daemonReachable && connectionElsewhere(pods.connected, pods.bleConnectionState,
    pods.lastAdvertisementMs, now, pods.disconnectedMs, pods.bluezConnected)
  var opening = openingControlLink(pods.connected, control, pods.fleet && pods.fleet.routing)
  var nearby = podsNearby(pods.lastAdvertisementMs, pods.lidState, now)
  var connectionVisible = !!previous || !!fleetView || !!(control && control.peer_reconnect) || pods.connecting
    || pods.holdingOmarchySeat || taking
    || (!telemetry && (pods.hasAirPods || nearby || pods.seatHold || pods.leftPod.inEar || pods.rightPod.inEar
      || pods.connectedPeers.length > 0))
  // Closing the lid can precede Bluetooth disconnecting. Keep the log while
  // either connection is still up; only then offer the lid-opening guidance.
  // Nearby advertisements identify the model, but do not complete local setup.
  var pairingRequired = pods.daemonReachable && (pods.bluetoothPaired === false || (!pods.pairingKeysReady
    && !pods.connected && !pods.bluezConnected && !opening && !pods.connecting && !taking))
  var guidanceVisible = pairingRequired || (caseClosed ? !pods.connected && !pods.bluezConnected : !connectionVisible)
  var idle = !taking && !pods.connecting && !pods.holdingOmarchySeat
  var localAudioResult = localAudio(pods.localAudioState, pods.localAudioFailure,
    pods.isHeadset || pods.leftPod.inEar || pods.rightPod.inEar, pods.localPlaybackConfirmed, hostLabel(pods.localName),
    audioOwner(pods).holder === "local" && !audioOwner(pods).playing)
  // The local output can disappear before an ownership handoff completes.
  // Keep the seat's known owner in the header unless an actual failure is reported.
  var outputUnavailableDuringHandoff = pods.localAudioState === "unavailable"
    && !audioFailure(pods.localAudioFailure) && !!seat.audioSubtitle
  var waitingForPlayback = !s.seatHandoff && pods.waitingForPlayback && !pods.connected && !pods.bluezConnected
    && !connectionAttempt(control) && !(control && control.peer_reconnect)
    && !(control && control.fleet_retry && control.fleet_retry.peer)
    && !(pods.fleet && pods.fleet.routing && pods.fleet.routing.busy)
  var connectionHold = control && !control.peer_reconnect && control.stage === "interrupted" ? { kind: "connection", connection: control }
    : waitingForPlayback ? { kind: "waitingForPlayback", device: hostLabel(pods.localName) } : null
  var caption = !pods.daemonReachable ? { kind: "offline" }
    : pairingRequired ? null
    : caseClosed ? { kind: "caseClosed" }
    : connectionHold ? connectionHold
    : s.joined ? s.joined
    : fleetView && fleetView.pending ? { kind: "fleetSwitching", device: fleetView.reporterLabel }
    : fleetView ? (fleetView.sourceLabel ? null : { kind: "reporterConnected", device: fleetView.reporterLabel })
    : previous && previous.handover ? { kind: "fleetSwitching", device: previous.peerLabel || "another laptop",
        keeper: previous.appleOwnsAudio ? previous.label : null }
    : previous && previous.unconfirmed ? { kind: "fleetUnconfirmed", device: previous.label }
    : controlDescription !== "" ? { kind: "connection", connection: control }
    : seat.refreshingConnection ? null
    : !pods.isHeadset && idle && inCase ? { kind: "inCase" }
    : opening ? null
    : elsewhere && idle ? { kind: "elsewhere", device: seat.appleSeatUnknown ? "" : elsewhereDevice(pods) }
    : telemetry && idle ? { kind: "unavailable", headset: pods.isHeadset }
    : !connectionVisible ? null
    : pods.remoteOmarchyAudio ? { kind: "audioOn", device: peerInfo(pods.remoteSourceAddress, pods.localAddress, pods.localName, pods.devices).label }
    : s.linuxHeld && pods.localAudioState && pods.localAudioState !== "playing" && pods.localAudioState !== "ready" ? null
    : !s.linuxHeld && seat.omarchyRosterOwner !== "" ? null
    : domainEvent(s.linuxHeld, seat.seatMap.apple, [{ label: hostLabel(pods.localName), onAcl: pods.connected || pods.bluezConnected }],
        pods.holdingOmarchySeat, !pods.connected, taking, seat.ownershipUnknown, pods.bluezConnected)
  var live = !pods.daemonReachable ? { kind: "offline" }
    : pairingRequired ? { kind: "pairingRequired" }
    : caseClosed ? { kind: "caseClosed" }
    : pods.transportPlaybackConfirmed ? { kind: "audio", text: playingLabel(hostLabel(pods.localName), "Omarchy") }
    : fleetView && !fleetView.pending ? { kind: "audio", text: seat.audioSubtitle }
    : seat.appleSeatUnknown && elsewhere ? { kind: "elsewhere" }
    : controlDescription !== "" ? { kind: control.stage === "waiting" ? "waiting"
      : control.stage === "interrupted" ? "notConnected"
      : control.stage === "failed" ? "connectionUnavailable"
      : control.recovering || control.stage === "retrying" ? "retrying" : "connecting" }
    : taking || pods.connecting ? { kind: "connecting" }
    : pods.holdingOmarchySeat ? { kind: "reconnecting" }
    : seat.refreshingConnection ? { kind: "refreshing" }
    : !pods.isHeadset && inCase ? { kind: "inCase" }
    : opening ? { kind: "connecting" }
    : elsewhere ? { kind: "elsewhere" }
    : telemetry ? { kind: "unavailable", headset: pods.isHeadset }
    : !pods.connected ? { kind: "notConnected" }
    : s.linuxHeld && !localAudioResult.fleet && !outputUnavailableDuringHandoff ? { kind: "audio", text: localAudioResult.text }
    : seat.audioSubtitle ? { kind: "audio", text: seat.audioSubtitle } : { kind: "connected" }
  // The seat and header use the same bounded evidence, never an independently
  // cached caption that can outlive the device and its playing animation.
  var carriedAudio = seat.audioSubtitle ? { kind: "audio", text: seat.audioSubtitle } : null
  var recoveringApple = !!s.presentation.audio && seat.appleOwnsAudio
    && !!control && control.stage !== "interrupted" && (control.recovering || controlOpening(control))
  var subtitle = pairingRequired || caseClosed ? live : heldSubtitle(seat.switching || recoveringApple, live, carriedAudio)
  s.caption = caption
  s.description = statusText(caption)
  s.subtitle = subtitleText(subtitle)
  s.showSeats = pods.daemonReachable || connectionVisible
  s.guidanceVisible = guidanceVisible
  s.pairingRequired = pairingRequired
  // Discovery is useful for setup instructions, not a populated connection diagram.
  s.seat = pairingRequired ? Object.assign({}, seat, {
    seats: { apple: [], omarchy: [] }, seatMap: { apple: [], omarchy: [] },
    availableApple: [], availableOmarchy: [],
    appleTrayAvailable: false, appleTrayRemembered: false, appleSeatUnknown: false,
    appleOwnsAudio: false, omarchyOwnsAudio: false, linuxHeld: false,
    connectedToOmarchy: false, viewOwnershipUnknown: true,
    audioPlaying: false, audioDeviceAddress: "", audioSubtitle: "",
    audioPeer: { label: "", address: "", kind: "" },
    switching: false, takingOverOmarchy: false, omarchyTransitionLabel: "",
    attempt: { kind: "none", label: "" }
  }) : seat
  var alongside = connectedFleetPeer(s.status)
  var routing = pods.fleet && pods.fleet.routing
  var roster = pods.fleet && pods.fleet.roster
  var admission = pods.daemonReachable && !pods.connected && routing && routing.busy
    && (routing.phase === "waiting_admission" || routing.phase === "connecting") && routing.peer ? {
      phase: routing.phase,
      peer: peerInfo(routing.peer, pods.localAddress, pods.localName, pods.devices).label,
      disconnect: joinReleased(pods) && ((roster && roster.audio_source && roster.audio_idle !== true)
        || (seat.appleOwnsAudio && seat.audioPlaying))
    } : null
  return {
    admission: admission,
    controlConnection: pods.daemonReachable ? control : null,
    connectedAlongside: alongside ? peerInfo(alongside, pods.localAddress, pods.localName, pods.devices).label : "",
    connectionHold: pods.daemonReachable ? connectionHold : null,
    fleetRetry: control && control.fleet_retry && control.fleet_retry.peer ? {
      peer: peerInfo(control.fleet_retry.peer, pods.localAddress, pods.localName, pods.devices).label,
      at_ms: control.fleet_retry.at_ms, exhausted: control.fleet_retry.exhausted
    } : null,
    fleetRetryStop: control && control.fleet_retry_stop && control.fleet_retry_stop.peer ? {
      peer: peerInfo(control.fleet_retry_stop.peer, pods.localAddress, pods.localName, pods.devices).label,
      reason: control.fleet_retry_stop.reason
    } : null,
    localDevice: hostLabel(pods.localName),
    audioFailure: audioFailure(pods.localAudioFailure),
    audioReady: pods.daemonReachable && (pods.localPlaybackConfirmed || pods.idleAudioReady),
    refreshing: seat.refreshingConnection,
    inCaseOnly: !pods.nativeHandoff && controlDescription === ""
      && (subtitle.kind === "inCase" || (caption !== null && caption.kind === "inCase")),
    suppress: statusLogSuppressed(guidanceVisible, subtitle.kind),
    caption: pods.statusLoaded && !(pods.nativeHandoff && caption
      && (caption.kind === "appleHasAudio" || caption.kind === "audioTaken" || caption.kind === "audioOn")) ? caption : null,
    audio: pods.daemonReachable ? pods.audioRoute.message || "" : "",
    audioEvent: pods.daemonReachable ? pods.audioRoute.event || null : null,
    phase: pods.nativeHandoff || controlDescription !== "" ? "" : handoffPhaseLabel(pods.handoffPhase),
    error: s.action !== "" ? s.action
      : (pods.daemonReachable && control ? control.peer_reconnect_error || "" : "") || pods.localAudioFailure
        || (pods.daemonReachable ? pods.lastError : "")
  }
}

function statusLogLastStatus(s) {
  for (var i = s.rows.length - 1; i >= 0; --i) if (s.rows[i].kind === "status") return s.rows[i].message
  return ""
}

// A row keeps the event it shows, so later decisions about the row read its kind, never its words.
function statusLogAppend(s, message, kind, live, detail, event) {
  s.rows.push({ id: s.nextId++, at: s.now, kind: kind, message: message, event: event || null, live: live === true,
    detail: detail || "", retryPeer: "", retryAt: 0 })
  if (s.rows.length > LOG_CAPACITY) s.rows.splice(0, s.rows.length - LOG_CAPACITY)
}

function statusLogNote(s, event, kind, live, detail) {
  statusLogAppend(s, statusText(event), kind, live, detail, event)
}

function statusLogReword(row, event, fields) {
  Object.assign(row, fields, { message: statusText(event), event: event })
}

function joinedAlongside(event, peer) {
  return !!event && event.kind === "joinedAlongside" && event.peer === peer
}

// Each connection attempt is one row: a retry rewrites it, success completes it, anything else closes it. Returns
// "open" while an attempt runs, "connected" or "ended" when one finishes, and "" otherwise.
function statusLogConnection(s, control, current, fleetRetry, connectionHold, alongside, admission, localDevice) {
  var attempt = connectionAttempt(control)
  if (control && control.stage === "failed" && !control.peer_reconnect && !s.caseRecoveryQuiet) s.connectionFailed = true
  var open = -1
  for (var i = s.rows.length - 1; i >= 0; --i) if (s.rows[i].live) { open = i; break }
  // Keep the first step while the helper's empty roster arrives. Only admission
  // (or an actual local attempt) advances it to Connecting, not that withdrawal.
  var openEvent = open >= 0 ? s.rows[open].event : null
  if (!attempt && admission && admission.phase === "connecting"
      && openEvent && (openEvent.kind === "waitingDisconnect" || openEvent.kind === "connecting"))
    attempt = { detail: "" }
  if (!attempt && admission && admission.phase === "waiting_admission" && (admission.disconnect
      || (openEvent && openEvent.kind === "waitingDisconnect" && openEvent.peer === admission.peer))) {
    var waiting = { kind: "waitingDisconnect", peer: admission.peer }
    if (open < 0) statusLogNote(s, waiting, "connection", true)
    else statusLogReword(s.rows[open], waiting, { detail: "", retryPeer: "", retryAt: 0 })
    return "open"
  }
  if (fleetRetry && fleetRetry.peer && !attempt) {
    s.waitingForPeer = null
    if (fleetRetry.exhausted) {
      if (open >= 0) statusLogReword(s.rows[open], { kind: "retriesExhausted", peer: fleetRetry.peer },
        { live: false, retryPeer: "", retryAt: 0, kind: "status" })
      return "ended"
    }
    if (open < 0) {
      var last = s.rows[s.rows.length - 1]
      var scheduled = { kind: "retryScheduled", peer: fleetRetry.peer }
      if (last && last.event && last.event.kind === "peerBusy" && last.event.peer === fleetRetry.peer) {
        open = s.rows.length - 1
        statusLogReword(last, scheduled, { kind: "connection", live: true })
      } else {
        statusLogNote(s, scheduled, "connection", true)
        open = s.rows.length - 1
      }
    }
    Object.assign(s.rows[open], { retryPeer: fleetRetry.peer, retryAt: Number(fleetRetry.at_ms || 0) })
    return "open"
  }
  if (open >= 0 && s.rows[open].retryPeer)
    statusLogReword(s.rows[open], { kind: "retryCancelled" }, { retryPeer: "", retryAt: 0 })
  if (attempt) {
    s.caseRecoveryQuiet = false
    var connecting = { kind: "connecting", detail: attempt.detail, device: localDevice || "" }
    if (open < 0) {
      if (s.connectionFailed) statusLogNote(s, { kind: "connectingAgain" }, "connection")
      s.connectionFailed = false
      statusLogNote(s, connecting, "connection", true, attempt.detail)
    } else statusLogReword(s.rows[open], connecting, { detail: attempt.detail })
    return "open"
  }
  var connected = !!current.connectionEvent && current.connectionEvent.kind === "connected"
  if (connected) { s.connectionFailed = false; s.caseRecoveryQuiet = false }
  var joined = connected && alongside ? { kind: "joinedAlongside", peer: alongside } : null
  if (open >= 0) {
    s.rows[open].live = false
    if (connectionHold) statusLogReword(s.rows[open], connectionHold, { kind: "status" })
    if (connected) {
      statusLogReword(s.rows[open], joined || { kind: "connected", fromAttempt: true }, joined ? { kind: "success" } : {})
      return "connected"
    }
  }
  if (current.connection && current.connection !== s.previous.connection) {
    if (joined) statusLogNote(s, joined, "success")
    else statusLogAppend(s, current.connection, "connection", false, "", current.connectionEvent)
  }
  return open >= 0 ? "ended" : connected ? "connected" : ""
}

function statusLogRecord(s, input) {
  var current = connectionLog(input.controlConnection, input.refreshing, input.caption, input.phase)
  var setStatus = function (event) { current.statusEvent = event; current.status = statusText(event) }
  if (input.connectionHold) setStatus(input.connectionHold)
  if (input.fleetRetryStop && input.fleetRetryStop.reason) {
    setStatus({ kind: "retryStopped", peer: input.fleetRetryStop.peer, reason: input.fleetRetryStop.reason })
    for (var row of s.rows) if (row.live && row.retryPeer)
      statusLogReword(row, { kind: "retryRequested", peer: row.retryPeer }, { live: false, retryPeer: "", retryAt: 0 })
  }
  var control = input.controlConnection
  // Case recovery stays quiet until a visible attempt or connection starts the new session.
  current.error = control && control.stage === "failed" ? (s.caseRecoveryQuiet ? "" : control.reason || "AirPods connection unavailable")
    : audioFailureMessage(input.error, control && control.stage === "ready", input.localDevice)
  if (input.fleetRetry && input.fleetRetry.peer && !input.fleetRetry.exhausted) current.error = ""
  // Apple Reconnect deliberately drops the local link to free a slot.
  if (control && control.peer_reconnect && current.error === "priority_connection_lost") current.error = ""
  current.audio = input.audio
  for (var kind of ["connection", "status", "audio", "phase", "error"]) current[kind] = String(current[kind] || "").trim()
  if (input.suppress) {
    s.rows = []
    s.connectionFailed = false
    s.caseRecoveryQuiet = true
    s.audioRecoveryPending = false
    s.previous = current
    return
  }
  if (input.inCaseOnly && !current.connection) {
    var inCase = { kind: "inCase" }
    s.connectionFailed = false
    s.caseRecoveryQuiet = true
    s.audioRecoveryPending = false
    var first = s.rows[0]
    if (!(s.rows.length === 1 && first.kind === "status" && first.event && first.event.kind === "inCase")) {
      s.rows = []
      statusLogNote(s, inCase, "status")
    }
    s.previous = { connection: current.connection, status: statusText(inCase), phase: "", error: "" }
    return
  }
  s.previous = Object.assign({}, s.previous)
  var outcome = statusLogConnection(s, control, current, input.fleetRetry, input.connectionHold, input.connectedAlongside, input.admission, input.localDevice)
  if (s.joinSuccess) {
    for (var i = s.rows.length - 1; i >= 0; --i) {
      var shown = s.rows[i].event
      if (shown && (shown.kind === "connected" || shown.kind === "joinedAlongside")) {
        statusLogReword(s.rows[i], s.joinSuccess, { kind: "success" })
        break
      }
    }
    if (i < 0) statusLogNote(s, s.joinSuccess, "success")
    if (joinedAlongside(current.statusEvent, s.joinSuccess.peer)) setStatus(null)
    s.joinSuccess = null
  }
  var statusEvent = current.statusEvent
  if (statusEvent && statusEvent.kind === "joinedAlongside" && s.rows.some(function (row) {
    return row.kind === "success" && joinedAlongside(row.event, statusEvent.peer)
  })) setStatus(null)
  if (s.waitingForPeer) {
    if (statusLogLastStatus(s) !== statusText(s.waitingForPeer)) statusLogNote(s, s.waitingForPeer, "status")
    s.waitingForPeer = null
  }
  var arrived = !s.handedOverAddress || (!(s.seat.fleetView && s.seat.fleetView.pending)
    && s.seat.seats.omarchy.some(function (device) {
      return device.onAcl === true && normalizeAddress(device.address) === s.handedOverAddress
    }))
  if (s.handedOver && arrived) {
    if (statusLogLastStatus(s) !== statusText(s.handedOver)) statusLogNote(s, s.handedOver, "status")
    s.handedOver = null
    s.handedOverAddress = ""; s.handedOverUntil = 0
  }
  if (s.switchFailed && outcome === "connected") s.switchFailed = null
  else if (s.switchFailed && outcome !== "open") {
    // One red line for the failed switch; the attempt's own failure reason is the same event.
    statusLogNote(s, s.switchFailed, "error")
    if (current.error && s.errors.indexOf(current.error) < 0) s.errors = s.errors.concat([current.error])
    s.switchFailed = null
  }
  if (input.fleetRetry && input.fleetRetry.peer) setStatus(null)
  s.previous.connection = current.connection
  for (var k of ["status", "audio", "phase", "error"]) {
    var message = current[k]
    var lastStatus = statusLogLastStatus(s)
    // An error shows once until the errors clear, so one that lingers cannot resurface each time a newer one passes.
    var shownError = k === "error" && s.errors.indexOf(message) >= 0
    if (message && message !== s.previous[k] && !shownError && (k !== "status" || statusCaptionChanged(lastStatus, message)))
      statusLogAppend(s, message, k, false, k === "error" ? input.error : "",
        k === "status" ? current.statusEvent : k === "audio" ? input.audioEvent : null)
    s.previous[k] = current[k]
  }
  if (input.audioFailure) s.audioRecoveryPending = true
  if (s.audioRecoveryPending && input.audioReady && !current.error) {
    statusLogNote(s, { kind: "audioRestored", device: input.localDevice || "AirPods" }, "success")
    s.audioRecoveryPending = false
  }
  s.errors = current.error ? (s.errors.indexOf(current.error) >= 0 ? s.errors : s.errors.concat([current.error])) : []
}
