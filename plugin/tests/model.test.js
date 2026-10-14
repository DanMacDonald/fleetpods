// Run with: deno run --allow-read plugin/tests/model.test.js
// Model.js has no exports, so it is evaluated here rather than imported.

const source = Deno.readTextFileSync(new URL("../Model.js", import.meta.url))
const Model = new Function(
  source + "; return { pairingWarning, deviceNameMarkup, logDeviceNames, appleTrayAvailable, fleetOutcome, fleetRouteFailed, statusLog, statusLogState, statusLogRecord, connectionAttempt, caseQuietConnection, connectingLine, seatPresentation, seatAudioSubtitle, localAudio, heldSubtitle, backgroundJoin, seatAudioOwner, fleetViewSubtitle, appleFleetSubtitle, fleetLabel, playingLabel, appleFleetMember, localSeatConnected, localConnectionLabel, fleetSeatView, remoteOmarchyAudio, omarchyRosterOwner, appleRosterOwner, appleOwnsAudio, confirmedLocalPlayback, idleAudioReady, controlConnectionLabel, audioRouteState, localAudioLabel, defaultStatus, connectionPresentation, connectionElsewhere, confirmedInCase, showDisconnectedMark, telemetryUnavailable, displayedBatteryLevel, ownershipText, parseStatus, podFrom, defaultPod, noiseModeVerb, earDetectionVerb, levelFraction, levelText, podMeta, elideError, availableModes, seatOccupants, availableDevices, linuxHeld, domainEvent, statusText, handoffPhaseLabel, podsNearby, appleOccupantAddress, applePadDevices, devicesMap, caseIconState, inCase, budsOut, emptyGuidance, statusLogSuppressed, iconVariant, openingControlLink, statusCaptionChanged, isPhone, appleSeat, omarchySeat, seatPairHold, seatedDevices, trayDevices, seatDeviceLit, NOISE_OFF, NOISE_ANC, NOISE_TRANSPARENCY, NOISE_ADAPTIVE, LEVEL_UNKNOWN, NOISE_UNKNOWN, EAR_PAUSE_ONE_OUT, LID_OPEN, LID_CLOSED, LID_UNKNOWN, MAX_ERROR_CHARS }"
)()

const PEERS = {
  "BB:BB:BB:BB:BB:01": { label: "Linux laptop", kind: "omarchy" },
  "AA:AA:AA:AA:AA:02": { label: "MacBook Pro", kind: "apple" },
  "AA:AA:AA:AA:AA:01": { label: "iPhone", kind: "apple" }
}

let failures = 0
const domainCaption = (...args) => Model.statusText(Model.domainEvent(...args))

function check(name, actual, expected) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected)
  if (!ok) {
    failures++
    console.log("FAIL " + name + "\n  expected " + JSON.stringify(expected) + "\n  got      " + JSON.stringify(actual))
  }
}

// The daemon resolves the owner from the pods' reports; daemon/tests/tst_ownership.cpp covers which report wins.
const LOCAL = { holder: "local", playing: false }, LOCAL_PLAYING = { holder: "local", playing: true }
const REMOTE = { holder: "remote", playing: false }, REMOTE_PLAYING = { holder: "remote", playing: true }
const UNKNOWN = { holder: "", playing: false }

// A peer returning from sleep can own audio while this connected laptop still
// has playback intent from an earlier play. The header follows the confirmed owner.
{
  const local = 'D0:C6:37:2F:D0:9F', xps = 'A8:72:4D:0C:7C:EE'
  const ready = { ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
    bluetoothPaired: true, pairingKeysReady: true, localAddress: local, localName: 'lg-gram',
    controlConnection: { stage: 'ready' }, ownershipIntent: 'claim', wantOmarchySeat: true,
    audioOwner: REMOTE_PLAYING, remoteSourceAddress: xps, rosterAudioOwner: xps,
    connectedPeers: [local, xps], peerListUpdated: 1000,
    devices: { [local]: { label: 'lg-gram', kind: 'omarchy' }, [xps]: { label: 'omarchy-xps', kind: 'omarchy' } },
    nativeHandoff: { connecting: false, attempting: false, phase: 'idle', playing: false, audio_state: 'unavailable' } }
  let state = Model.statusLog(Model.statusLogState(), { t: 1000, ev: 'status', status: ready })
  check('a ready Gram with old playback intent is not taking a connection', state.taking, false)
  check('a returned XPS owner replaces Connecting in Gram header', state.subtitle, 'Omarchy fleet · omarchy-xps · Playing')
  state = Model.statusLog(state, { t: 2000, ev: 'status', status: { ...ready, audioOwner: REMOTE } })
  check('an idle returned XPS owner keeps its fleet name', state.subtitle, 'Omarchy fleet · omarchy-xps')
}

check("local ownership", Model.ownershipText({ connected: true, audioOwner: LOCAL }), "Bluetooth connected · Audio owned by Linux")
check("remote ownership", Model.ownershipText({ connected: true, audioOwner: REMOTE_PLAYING }), "Bluetooth connected · Audio owned by another device")
check("unknown ownership", Model.ownershipText({ connected: true, audioOwner: UNKNOWN }), "Bluetooth connected · Audio ownership unknown")
check("disconnected", Model.ownershipText({ connected: false, audioOwner: LOCAL }), "Bluetooth disconnected")
check("the owner is parsed from status", Model.parseStatus(JSON.stringify({ schema_version: 1,
  audio_owner: { holder: "remote", playing: true } })).audioOwner, REMOTE_PLAYING)
check("an unrecognized owner is unknown", Model.parseStatus(JSON.stringify({ schema_version: 1,
  audio_owner: { holder: "phone", playing: 1 } })).audioOwner, UNKNOWN)
check("an older daemon's status has no owner", Model.parseStatus('{"schema_version":1}').audioOwner, UNKNOWN)

// A status without fleet_outcome, as the 2026-10-07 replays recorded, is read from the routing phase leaving busy.
const routing = (phase, busy, error) => ({ ok: true, fleet: { phase: "idle", routing: { phase: phase, busy: busy, error: error || "" } } })
const ended = (before, after) => Model.fleetRouteFailed(Model.fleetOutcome(before, after))
check("a route that fails ends the activity hold", ended(routing("waiting_admission", true), routing("failed", false, "peer_busy")), true)
check("a refused admission ends it too", ended(routing("admitting", true), routing("failed", false, "timed_out")), true)
check("a route still waiting keeps the hold", ended(routing("waiting_admission", true), routing("connecting", true)), false)
check("a route that succeeds keeps the hold", ended(routing("waiting_completion", true), routing("success", false)), false)
check("an earlier failure still in status is not a new one", ended(routing("failed", false), routing("failed", false)), false)
check("a status without routing is not a failure", ended({ ok: true, fleet: { phase: "idle" } }, { ok: true, fleet: { phase: "idle" } }), false)
const outcome = (atMs, result) => ({ atMs, result, peer: "02:00:00:00:00:01", retain: "02:00:00:00:00:04" })
check("a daemon outcome is read once", [
  Model.fleetOutcome({ ok: true, fleetOutcome: null }, { fleetOutcome: outcome(5, "peer_busy") }),
  Model.fleetOutcome({ ok: true, fleetOutcome: outcome(5, "peer_busy") }, { fleetOutcome: outcome(5, "peer_busy") }),
  Model.fleetOutcome({ ok: true, fleetOutcome: outcome(5, "peer_busy") }, { fleetOutcome: outcome(6, "joined") })],
  [outcome(5, "peer_busy"), null, outcome(6, "joined")])
check("an outcome already in the first status is not news",
  Model.fleetOutcome(Model.defaultStatus(), { fleetOutcome: outcome(5, "admitted") }), null)
check("a daemon outcome is used even when the busy phase was never seen",
  Model.fleetOutcome({ ok: true, fleet: { routing: { phase: "success", busy: false } } },
    { ...routing("success", false), fleetOutcome: outcome(7, "joined_alongside") }).result, "joined_alongside")
check("every failing result ends the hold, no success does",
  ["peer_busy", "peer_unavailable", "join_failed", "admit_failed", "joined", "joined_alongside", "admitted", "admitted_alongside"]
    .map(r => Model.fleetRouteFailed({ result: r })), [true, true, true, true, false, false, false, false])
check("fleet_outcome is parsed from status", Model.parseStatus(JSON.stringify({ schema_version: 1,
  fleet_outcome: { at_ms: 1760140800000, result: "admitted", peer: "02:00:00:00:00:0a", retain: "02:00:00:00:00:04" } })).fleetOutcome,
  { atMs: 1760140800000, result: "admitted", peer: "02:00:00:00:00:0A", retain: "02:00:00:00:00:04" })
check("an older daemon's status has no fleet outcome", Model.parseStatus('{"schema_version":1}').fleetOutcome, null)

// Fixture with a curly apostrophe in the device name, which the panel must keep.
const live = '{"adaptive_level_changes_total":0,"adaptive_noise_level":50,"ca_changes_total":0,"case":{"available":true,"charging":false,"level":100},"connect_calls_total":0,"connect_failures_total":0,"connected":true,"conversational_awareness":true,"device_name":"Owner’s AirPods Pro","disconnect_calls_total":0,"disconnect_failures_total":0,"ear_detection_behavior":0,"ear_detection_changes_total":0,"forget_calls_total":0,"is_pro_series":true,"left":{"available":true,"charging":false,"in_ear":false,"level":79},"lid_state":2,"model_int":11,"model_name":"AirPods Pro 3","model_number":"A3064","noise_control_changes_total":0,"noise_mode":1,"one_bud_anc_changes_total":0,"one_bud_anc_mode":true,"reconnect_attempts_total":0,"reconnect_failures_total":0,"reopen_calls_total":0,"right":{"available":true,"charging":true,"in_ear":false,"level":100},"schema_version":1,"supports_noise_off":false}'

const good = Model.parseStatus(live)
check("live line parses", good.ok, true)
check("live modelName", good.modelName, "AirPods Pro 3")
check("live peers default empty", good.connectedPeers, [])

const seats = Model.seatOccupants(
  ["AA:AA:AA:AA:AA:02", "BB:BB:BB:BB:BB:01"],
  "BB:BB:BB:BB:BB:01",
  "linux-laptop",
  PEERS
)
check("apple seat label", seats.apple[0].label, "MacBook Pro")
check("iphone waits when mac is seated",
  Model.availableDevices("apple", ["AA:AA:AA:AA:AA:02", "BB:BB:BB:BB:BB:01"], PEERS)[0].label,
  "iPhone")
check("no extra omarchy while the laptop is seated",
  Model.availableDevices("omarchy", ["AA:AA:AA:AA:AA:02", "BB:BB:BB:BB:BB:01"], PEERS),
  [])
check("omarchy seat label", seats.omarchy[0].label, "linux-laptop")
const cachedDevices = Model.devicesMap([
  { address: "02:00:00:00:00:04", label: "02-00-00-00-00-04", kind: "apple" },
  { address: "02:00:00:00:00:05", label: "02:00:00:00:00:05", kind: "apple" },
  { address: "02:00:00:00:00:06", label: "Apple device", kind: "apple" },
  { address: "02:00:00:00:00:07", label: "Owner’s iPhone", kind: "apple" }
])
check("unnamed cached devices are not reconnect candidates",
  Model.availableDevices("apple", [], cachedDevices).map(d => d.label), ["Owner’s iPhone"])
check("an address-only cached peer is not presented as Apple",
  Model.applePadDevices(["02:00:00:00:00:04"], cachedDevices, ""), [])
check("an unidentified connection has no Apple icon", Model.applePadDevices(["02:00:00:00:00:09"], {}, "02:00:00:00:00:09"), [])
const identifiedHosts = Model.devicesMap([
  { address: "02:00:00:00:00:04", label: "Nearby advertisement", kind: "apple" },
  { address: "02:00:00:00:00:07", label: "iPhone", kind: "apple", model: "iPhone15,2" }
])
check("identified host precedes an unverified scan name",
  Model.availableDevices("apple", [], identifiedHosts)[0].label, "iPhone")
const discoveredAddress = "02:00:00:00:00:04"
const nearbyMember = Model.devicesMap([{ address: discoveredAddress, label: "peer-laptop", kind: "omarchy", nearby: true }])
check("nearby enrolled peer waits above its seat", Model.availableDevices("omarchy", [], nearbyMember).map(d => d.label), ["peer-laptop"])
check("nearby enrolled peer has no plus", Model.availableDevices("omarchy", [], nearbyMember)[0].discovered, undefined)
check("roster moves a nearby peer onto its seat", Model.seatOccupants([discoveredAddress], "", "", nearbyMember).omarchy.map(d => d.label), ["peer-laptop"])
check("seated peer is not also hovering", Model.availableDevices("omarchy", [discoveredAddress], nearbyMember), [])
const absentMember = Model.devicesMap([{ address: discoveredAddress, label: "peer-laptop", kind: "omarchy", nearby: false }])
check("enrolled peer stays in the tray without nearby presence", Model.availableDevices("omarchy", [], absentMember).map(d => d.label), ["peer-laptop"])
check("absent enrolled peer does not gain an invitation button", Model.availableDevices("omarchy", [], absentMember)[0]?.discovered, undefined)
const absentDiscovery = Model.devicesMap([{ address: discoveredAddress, label: "peer-laptop", kind: "omarchy", nearby: false, discovered: true }])
check("expired discovery hides an unenrolled peer", Model.availableDevices("omarchy", [], absentDiscovery), [])
check("connected roster remains authoritative after BLE expiry", Model.seatOccupants([discoveredAddress], "", "", absentMember).omarchy.map(d => d.label), ["peer-laptop"])
const discoveredDevices = Model.devicesMap([
  { address: discoveredAddress, label: "omarchy-xps", kind: "omarchy", discovered: true }
])
const discoveredSeats = Model.seatOccupants([discoveredAddress], "02:00:00:00:00:01",
  "lg-gram", discoveredDevices, true, discoveredAddress)
check("discovered Omarchy is not an Apple occupant", discoveredSeats.apple, [])
check("an unjoined laptop stays out of the seat", discoveredSeats.omarchy.map(d => d.label), ["lg-gram"])
const discoveredWaiting = Model.availableDevices("omarchy", [discoveredAddress], discoveredDevices,
  "02:00:00:00:00:01", discoveredAddress)
check("connected discovery stays in the tray", discoveredWaiting.map(d => d.label), ["omarchy-xps"])
check("connected discovery retains Add to fleet", discoveredWaiting[0].discovered, true)
const resetPeer = Model.devicesMap([
  { address: discoveredAddress, label: "omarchy-xps", kind: "omarchy", discovered: true, nearby: true, fleet_enrolled: false }
])
check("reset peer with a Bluetooth connection remains outside the fleet seat",
  Model.seatOccupants([discoveredAddress], "", "", resetPeer).omarchy, [])
check("reset peer in the tray offers Add rather than Join",
  Model.availableDevices("omarchy", [discoveredAddress], resetPeer)[0].fleetEnrolled, false)
const disconnectedDiscovery = Model.availableDevices("omarchy", [], discoveredDevices,
  "02:00:00:00:00:01", "")
check("disconnected discovery remains available", disconnectedDiscovery.map(d => d.label), ["omarchy-xps"])
check("disconnected discovery retains its plus marker", disconnectedDiscovery[0]?.discovered, true)
const localListed = Model.seatOccupants(["02:00:00:00:00:01", discoveredAddress],
  "02:00:00:00:00:01", "lg-gram", discoveredDevices, true, discoveredAddress)
check("the local laptop in the pods roster never becomes an Apple occupant", localListed.apple, [])
check("only the local laptop sits before joining", localListed.omarchy.map(d => d.label), ["lg-gram"])
const afterReconnectDevices = Object.assign({}, discoveredDevices, PEERS)
const afterReconnectPeers = ["AA:AA:AA:AA:AA:01"]
check("discovered laptop remains available after iPhone reconnects",
  Model.availableDevices("omarchy", afterReconnectPeers, afterReconnectDevices,
    "BB:BB:BB:BB:BB:01", afterReconnectPeers[0]), disconnectedDiscovery)
check("disconnected discovery does not occupy the Omarchy seat",
  Model.seatOccupants(afterReconnectPeers, "BB:BB:BB:BB:BB:01", "lg-gram",
    afterReconnectDevices, true, afterReconnectPeers[0]).omarchy.map(d => d.label), ["lg-gram"])
const titled = Model.seatOccupants([], "BB:BB:BB:BB:BB:02", "linux-laptop", {})
const mislabeledLocal = { "02:00:00:00:00:01": { label: "Apple device", kind: "apple" } }
const correctedLocal = Model.seatOccupants([], "02:00:00:00:00:01", "linux-laptop", mislabeledLocal, true)
check("local adapter overrides a learned Apple label", correctedLocal.omarchy[0].label, "linux-laptop")
check("local adapter always belongs to Omarchy", correctedLocal.omarchy[0].kind, "omarchy")
check("local adapter is never an available Apple peer",
  Model.availableDevices("apple", [], mislabeledLocal, "02:00:00:00:00:01", ""), [])
check("an unlisted hostname keeps its spelling", titled.omarchy[0].label, "linux-laptop")
check("an empty hostname falls back to Omarchy",
  Model.seatOccupants([], "BB:BB:BB:BB:BB:02", "", {}).omarchy[0].label, "Omarchy")
const nearbySeats = Model.seatOccupants(["AA:AA:AA:AA:AA:01"], "BB:BB:BB:BB:BB:01", "linux-laptop", PEERS, false)
check("disconnected laptop leaves the Omarchy seat", nearbySeats.omarchy, [])
const fromSource = Model.seatOccupants([], "BB:BB:BB:BB:BB:01", "linux-laptop", PEERS, true, "AA:AA:AA:AA:AA:01")
check("iphone on apple pad from last source", fromSource.apple[0].label, "iPhone")
check("remembered iPhone has no confirmed connection", fromSource.apple[0].onAcl, false)
check("live Apple peer has a confirmed connection", seats.apple[0].onAcl, true)
check("iphone not waiting when it is the source",
  Model.availableDevices("apple", [], PEERS, "BB:BB:BB:BB:BB:01", "AA:AA:AA:AA:AA:01")[0].label,
  "MacBook Pro")
check("disconnected local laptop waits above the seat",
  Model.availableDevices("omarchy", [], PEERS, "BB:BB:BB:BB:BB:01", "", false).map(x => x.label), ["Linux laptop"])
check("nearby from fresh ad", Model.podsNearby(1000, 0, 2000, 20000), true)
check("not nearby when lid closed", Model.podsNearby(1000, Model.LID_CLOSED, 2000, 20000), false)
check("in case when lid closed and not in ear",
  Model.inCase(Model.LID_CLOSED, { inEar: false, charging: false, level: 80 }, { inEar: false, charging: false, level: 80 }, { level: 50 }),
  true)
check("out of case when in ear",
  Model.inCase(Model.LID_CLOSED, { inEar: true, charging: false, level: 80 }, { inEar: false, charging: false, level: 80 }, { level: 50 }),
  false)
check("in case when ads go stale and not in ear",
  Model.inCase(Model.LID_OPEN, { inEar: false, charging: false, level: 80 }, { inEar: false, charging: false, level: 80 }, { level: 18 }, 1000, 25000, 20000),
  true)
check("not in case when ads are fresh and lid open",
  Model.inCase(Model.LID_OPEN, { inEar: false, charging: false, level: 80 }, { inEar: false, charging: false, level: 80 }, { level: 18 }, 20000, 21000, 20000),
  false)
check("stale open lid is not out of case",
  Model.budsOut(Model.LID_OPEN, { inEar: false, charging: false, level: 0 }, { inEar: false, charging: false, level: 0 }, 1000, 300000, false, 20000),
  false)
check("in ear is out of case even if ads are stale",
  Model.budsOut(Model.LID_OPEN, { inEar: true, charging: false, level: 80 }, { inEar: false, charging: false, level: 80 }, 1000, 300000, false, 20000),
  true)
const rosterPeers = {
  "AA:AA:AA:AA:AA:01": { label: "Laptop", kind: "omarchy" },
  "BB:BB:BB:BB:BB:02": { label: "Mac", kind: "apple" },
  "CC:CC:CC:CC:CC:03": { label: "Phone", kind: "apple" }
}
for (const connected of [false, true]) {
  const occupants = Model.seatOccupants(["BB:BB:BB:BB:BB:02"], "AA:AA:AA:AA:AA:01",
    "Laptop", rosterPeers, connected, "CC:CC:CC:CC:CC:03")
  check("connected peer takes precedence over remembered source, connected=" + connected,
    occupants.apple.map(d => d.label), ["Mac"])
  check("local seat reflects connection, connected=" + connected,
    occupants.omarchy.length > 0 && occupants.omarchy[0].onAcl, connected)
}
check("other Apple device waits",
  Model.availableDevices("apple", ["BB:BB:BB:BB:BB:02"], rosterPeers,
    "AA:AA:AA:AA:AA:01", "CC:CC:CC:CC:CC:03").map(d => d.label), ["Phone"])
check("roster supplies the Apple seat when source is absent",
  Model.appleOccupantAddress(["BB:BB:BB:BB:BB:02"], rosterPeers, ""), "BB:BB:BB:BB:BB:02")
check("empty roster and source leave the Apple seat empty",
  Model.applePadDevices([], rosterPeers, ""), [])
check("devices list maps labels without hardcoded peers",
  Model.parseStatus('{"schema_version":1,"connected":false,"devices":[{"address":"aa:aa:aa:aa:aa:01","label":"iPad","kind":"apple"}]}').devices["AA:AA:AA:AA:AA:01"].label,
  "iPad")
check("nearby disconnected caption",
  domainCaption(false, [{ label: "iPhone" }], [{ label: "Linux laptop" }], false, true),
  "Linux laptop is not connected to the AirPods.")
check("holding seat caption",
  domainCaption(false, [{ label: "iPhone" }], [{ label: "Linux laptop" }], true),
  "Reclaiming Omarchy seat. Apple keeps audio.")
check("handoff phase names the audio profile step", Model.handoffPhaseLabel("profile_connect"), "Connecting the audio profile")
check("handoff phase names speaker recovery", Model.handoffPhaseLabel("recovery_speakers"), "Moving audio to the speakers")
check("unknown handoff phase has no label", Model.handoffPhaseLabel("idle"), "")
check("click omarchy connecting caption",
  domainCaption(false, [{ label: "MacBook Pro" }, { label: "iPhone" }], [{ label: "Linux laptop" }], true, true, true),
  "Linux laptop claims ownership.")
check("linux held caption names the seated apple device",
  domainCaption(true, [{ label: "MacBook Pro", onAcl: true }], [{ label: "Linux laptop" }]),
  "AirPods audio is on Linux laptop.")
check("remembered phone does not promise automatic switching",
  domainCaption(true, fromSource.apple, fromSource.omarchy),
  "AirPods audio is on linux-laptop.")
check("linux held by the resolved owner", Model.linuxHeld({ connected: true, audioOwner: LOCAL }), true)
check("apple held when the owner is remote", Model.linuxHeld({ connected: true, audioOwner: REMOTE_PLAYING }), false)
check("no owner without a link", Model.linuxHeld({ connected: false, audioOwner: LOCAL }), false)
check("live deviceName keeps the daemon's apostrophe", good.deviceName, "Owner’s AirPods Pro")
check("live left level", good.left.level, 79)
check("live case has no in_ear", good.caseBattery, { level: 100, charging: false })
check("live supportsNoiseOff is honoured", good.supportsNoiseOff, false)
check("live noiseMode", good.noiseMode, 1)

// A fresh daemon omits left, right and case entirely rather than sending available:false.
const fresh = Model.parseStatus('{"connected":false,"noise_mode":-1,"schema_version":1}')
check("fresh line parses", fresh.ok, true)
check("fresh left is the default pod", fresh.left, Model.defaultPod())
check("fresh case is unknown", fresh.caseBattery.level, Model.LEVEL_UNKNOWN)
check("supports_noise_off absent means the model has Off", fresh.supportsNoiseOff, true)

// available:false means the daemon stopped hearing from the pod, so no flag survives.
const gone = Model.podFrom({ available: false, level: 82, charging: true, in_ear: true })
check("unavailable pod reports no level", gone.level, Model.LEVEL_UNKNOWN)
check("unavailable pod reports no charging", gone.charging, false)
check("unavailable pod reports no in_ear", gone.inEar, false)
check("unavailable pod shows no meta", Model.podMeta(gone), "")

// Every failure path returns the full default shape rather than throwing.
const empty = Model.parseStatus("")
check("empty input is not ok", empty.ok, false)
check("empty input names the failure", empty.lastError !== "", true)
check("empty input still has a left pod", empty.left, Model.defaultPod())

const garbage = Model.parseStatus("not json at all")
check("garbage is not ok", garbage.ok, false)
check("garbage is not schemaTooNew", garbage.schemaTooNew, false)

const noVersion = Model.parseStatus('{"connected":true}')
check("a line with no schema_version is not ok", noVersion.ok, false)

const tooNew = Model.parseStatus('{"schema_version":2,"connected":true}')
check("a newer schema is not ok", tooNew.ok, false)
check("a newer schema is flagged", tooNew.schemaTooNew, true)
check("a newer schema reports both versions", tooNew.lastError, "FleetPods speaks status schema 2, this panel reads 1")

check("null parses to the default shape", Model.parseStatus("null").ok, false)

// Verbs, and the out-of-range guards that keep a bad index off the command line.
check("noise verb for Adaptive", Model.noiseModeVerb(3), "noise:adaptive")
check("noise verb for an unknown mode is empty", Model.noiseModeVerb(Model.NOISE_UNKNOWN), "")
check("noise verb past the end is empty", Model.noiseModeVerb(4), "")
check("ear verb for never pause", Model.earDetectionVerb(2), "ear:off")
check("ear verb past the end is empty", Model.earDetectionVerb(3), "")

// Meter and label edges.
check("an unknown level draws an empty track", Model.levelFraction(Model.LEVEL_UNKNOWN), 0)
check("a level above 100 is clamped", Model.levelFraction(140), 1)
check("a negative level is clamped", Model.levelFraction(-5), 0)
check("an unknown level reads as dashes", Model.levelText(Model.LEVEL_UNKNOWN), "--")

// Errors are elided to one line rather than dumped.
const long = Model.elideError("x\n\n   y".repeat(60))
check("an elided error is one line", long.indexOf("\n"), -1)
check("an elided error fits the row", long.length <= Model.MAX_ERROR_CHARS, true)
check("elideError copes with nothing", Model.elideError(null), "")

// A Max sends one battery under "headset" and no pods at all, so the panel needs the flag to pick a shape.
const max = Model.parseStatus('{"connected":true,"device_name":"AirPods Max","headset":{"available":true,"charging":false,"level":100},"is_headset":true,"model_name":"AirPods Max","noise_mode":1,"schema_version":1}')
check("max line parses", max.ok, true)
check("max is flagged a headset", max.isHeadset, true)
check("max headset level", max.headset, { level: 100, charging: false })
check("max has no pods", max.left.level, Model.LEVEL_UNKNOWN)
check("max has no case", max.caseBattery.level, Model.LEVEL_UNKNOWN)

// Between connect and the first battery packet the daemon sends the flag with no headset object at all.
const maxFresh = Model.parseStatus('{"connected":true,"is_headset":true,"noise_mode":1,"schema_version":1}')
check("max before any battery packet is still a headset", maxFresh.isHeadset, true)
check("max before any battery packet has no level", maxFresh.headset.level, Model.LEVEL_UNKNOWN)

// A daemon too old to send is_headset must keep drawing the pod rows rather than an empty headset row.
check("no is_headset key means earbuds", good.isHeadset, false)
check("no headset key is unknown, not zero", good.headset.level, Model.LEVEL_UNKNOWN)

// The capability keys, which are what stops the panel offering a control the hardware ignores.
const ap4 = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":false,"model_name":"AirPods 4","noise_mode":-1,"schema_version":1,"supports_adaptive":false,"supports_conversational_awareness":false,"supports_noise_control":false,"supports_noise_off":true,"supports_one_bud_anc":false}')
check("plain AirPods 4 has no listening modes", ap4.supportsNoiseControl, false)
check("plain AirPods 4 has no adaptive", ap4.supportsAdaptive, false)

const ap4anc = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":false,"model_name":"AirPods 4","noise_mode":1,"schema_version":1,"supports_adaptive":true,"supports_conversational_awareness":true,"supports_noise_control":true,"supports_noise_off":true,"supports_one_bud_anc":true}')
check("AirPods 4 with ANC hardware gets adaptive despite not being Pro", ap4anc.supportsAdaptive, true)
check("AirPods 4 with ANC hardware gets conversation awareness", ap4anc.supportsConversationalAwareness, true)
check("AirPods 4 with ANC hardware gets one-bud ANC", ap4anc.supportsOneBudANC, true)

const max2 = Model.parseStatus('{"connected":true,"is_headset":true,"is_pro_series":false,"model_name":"AirPods Max 2","noise_mode":1,"schema_version":1,"supports_adaptive":true,"supports_conversational_awareness":true,"supports_noise_control":true,"supports_noise_off":true,"supports_one_bud_anc":false}')
check("Max 2 has adaptive", max2.supportsAdaptive, true)
check("Max 2 has no second bud to keep ANC on", max2.supportsOneBudANC, false)

// A daemon older than the capability keys falls back to the Pro flag, which is what it used to gate on.
check("older daemon, Pro device, keeps adaptive", good.supportsAdaptive, true)
check("older daemon, Pro device, keeps one-bud ANC", good.supportsOneBudANC, true)
check("older daemon assumes listening modes exist", good.supportsNoiseControl, true)
const oldPlain = Model.parseStatus('{"connected":true,"is_pro_series":false,"noise_mode":1,"schema_version":1}')
check("older daemon, non-Pro device, hides adaptive", oldPlain.supportsAdaptive, false)
check("a false capability key is not treated as absent", ap4.supportsNoiseControl, false)

// An AirPods Pro 1 is the case the capability keys exist for: Pro, and no Adaptive.
const pro1 = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":true,"model_name":"AirPods Pro","noise_mode":1,"schema_version":1,"supports_adaptive":false,"supports_conversational_awareness":false,"supports_noise_control":true,"supports_noise_off":true,"supports_one_bud_anc":true}')
check("a Pro with an explicit false keeps the key, not the Pro flag", pro1.supportsAdaptive, false)
check("the same Pro still gets conversation awareness taken away", pro1.supportsConversationalAwareness, false)

// Three booleans rather than a status object, the shape Service.qml calls it with.
function modesFor(status) {
  return Model.availableModes(status.supportsNoiseControl, status.supportsNoiseOff, status.supportsAdaptive)
}
check("no listening modes at all on a plain AirPods 4", modesFor(ap4), [])
check("a Pro 1 gets Off, Transparency and ANC but no Adaptive", modesFor(pro1),
  [Model.NOISE_OFF, Model.NOISE_TRANSPARENCY, Model.NOISE_ANC])
check("a Pro 3 loses Off and keeps Adaptive", modesFor(good),
  [Model.NOISE_TRANSPARENCY, Model.NOISE_ADAPTIVE, Model.NOISE_ANC])
check("a Max 2 gets all four", modesFor(max2),
  [Model.NOISE_OFF, Model.NOISE_TRANSPARENCY, Model.NOISE_ADAPTIVE, Model.NOISE_ANC])

const restingPod = { inEar: false, charging: false }
const chargingPod = { inEar: false, charging: true }
const wornPod = { inEar: true, charging: false }
check("no disconnect mark without bud telemetry", Model.showDisconnectedMark(false, null, null, 1000, 2000), false)
check("no disconnect mark for resting buds", Model.showDisconnectedMark(false, restingPod, restingPod, 1000, 2000), false)
check("no disconnect mark for charging buds", Model.showDisconnectedMark(false, chargingPod, chargingPod, 1000, 2000), false)
check("fresh worn bud shows disconnect mark", Model.showDisconnectedMark(false, wornPod, chargingPod, 1000, 2000), true)
check("either worn bud shows disconnect mark", Model.showDisconnectedMark(false, chargingPod, wornPod, 1000, 2000), true)
check("connected worn buds have no disconnect mark", Model.showDisconnectedMark(true, wornPod, wornPod, 1000, 2000), false)
check("stale worn report has no disconnect mark", Model.showDisconnectedMark(false, wornPod, wornPod, 1000, 21000), false)
check("undated worn report has no disconnect mark", Model.showDisconnectedMark(false, wornPod, wornPod, 0, 2000), false)

check("closed charging pods confirm in-case availability", Model.confirmedInCase(Model.LID_CLOSED, chargingPod, chargingPod), true)
check("explicit docked flags confirm in-case availability", Model.confirmedInCase(Model.LID_CLOSED, { inCase: true }, { inCase: true }), true)
check("one charging pod does not confirm both docked", Model.confirmedInCase(Model.LID_CLOSED, chargingPod, restingPod), false)
check("in-ear evidence overrides charging", Model.confirmedInCase(Model.LID_CLOSED, { charging: true, inEar: true }, chargingPod), false)
check("unknown lid is not a confirmed closed case", Model.confirmedInCase(Model.LID_UNKNOWN, chargingPod, chargingPod), false)
check("open lid is not a confirmed closed case", Model.confirmedInCase(Model.LID_OPEN, chargingPod, chargingPod), false)
check("missing telemetry is not confirmed docked", Model.confirmedInCase(Model.LID_CLOSED), false)
check("closed lid selects closed case", Model.caseIconState(false, Model.LID_CLOSED, restingPod, restingPod), "closed")
check("open lid and both charging selects open case", Model.caseIconState(false, Model.LID_OPEN, chargingPod, chargingPod), "open")
check("open lid with buds in case selects open case", Model.caseIconState(false, Model.LID_OPEN, restingPod, restingPod), "open")
check("left in ear shows the right pod remaining in the case", Model.caseIconState(false, Model.LID_CLOSED, wornPod, chargingPod), "open-right")
check("right in ear shows the left pod remaining in the case", Model.caseIconState(false, Model.LID_OPEN, chargingPod, wornPod), "open-left")
check("both worn buds use no case", Model.caseIconState(false, Model.LID_OPEN, wornPod, wornPod), "")
// 2026-10-04: lg-gram drew both buds worn while the right one sat in a flat case and the iPhone played.
check("a bud that reports neither ear nor case stays in the case", Model.caseIconState(false, Model.LID_UNKNOWN,
  wornPod, restingPod, "", true, 1000, 2000, 20000, false), "open-right")
check("second in-ear from a half-empty case shows the empty case", Model.caseIconState(false, Model.LID_OPEN, wornPod, wornPod, "open-left"), "open-empty")
check("second in-ear from a full open case shows the empty case", Model.caseIconState(false, Model.LID_OPEN, wornPod, wornPod, "open"), "open-empty")
check("empty case holds while both stay in-ear", Model.caseIconState(false, Model.LID_OPEN, wornPod, wornPod, "open-empty"), "open-empty")
check("both worn from a closed case skip the empty flash", Model.caseIconState(false, Model.LID_CLOSED, wornPod, wornPod, "closed"), "")
check("unknown lid defaults to closed case", Model.caseIconState(false, Model.LID_UNKNOWN, chargingPod, chargingPod), "closed")
check("headphones never use a case icon", Model.caseIconState(true, Model.LID_CLOSED, restingPod, restingPod), "")
check("missing telemetry uses closed case", Model.caseIconState(false, Model.LID_UNKNOWN), "closed")
check("disconnected startup without an advertisement shows the closed case", Model.caseIconState(false,
  Model.LID_UNKNOWN, Model.defaultPod(), Model.defaultPod(), "", false, 0, 0, 20000, false, false), "closed")
check("connected worn pods do not require an advertisement", Model.caseIconState(false,
  Model.LID_UNKNOWN, wornPod, wornPod, "", true, 0, 0, 20000, true, true), "")

check("charging buds retain open case through unknown lid", Model.caseIconState(false, Model.LID_UNKNOWN, chargingPod, chargingPod, "open"), "open")
check("charging buds retain closed case through unknown lid", Model.caseIconState(false, Model.LID_UNKNOWN, chargingPod, chargingPod, "closed"), "closed")
check("one charging bud retains open case", Model.caseIconState(false, Model.LID_UNKNOWN, restingPod, chargingPod, "open"), "open")
check("left in ear keeps the remaining-right case through unknown lid", Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, chargingPod, "open"), "open-right")
check("known closed lid replaces retained open case", Model.caseIconState(false, Model.LID_CLOSED, chargingPod, chargingPod, "open"), "closed")

check("left bud in open case", Model.caseIconState(false, Model.LID_OPEN, chargingPod, restingPod), "open")
check("right bud in open case", Model.caseIconState(false, Model.LID_OPEN, restingPod, chargingPod), "open")
check("removing both buds keeps last case", Model.caseIconState(false, Model.LID_UNKNOWN, restingPod, restingPod, "open"), "open")
check("second pod out of ear shows the open case with both pods", Model.caseIconState(false, Model.LID_UNKNOWN, restingPod, restingPod, "open-left"), "open")
check("second pod out from the other side shows the open case", Model.caseIconState(false, Model.LID_UNKNOWN, chargingPod, chargingPod, "open-right"), "open")
check("both out of ears from the buds glyph shows the open case", Model.caseIconState(false, Model.LID_UNKNOWN, restingPod, restingPod, "", true), "open")
check("unknown lid without in-ear history stays closed", Model.caseIconState(false, Model.LID_UNKNOWN, restingPod, restingPod, "", false), "closed")
check("stale open lid with pods put away is a closed case", Model.caseIconState(false, Model.LID_OPEN, restingPod, chargingPod, "open", true, 1000, 30000), "closed")
check("connected quiet case shows closed without waiting for disconnect", Model.caseIconState(false, Model.LID_OPEN, restingPod, restingPod, "open", false, 1000, 30000, 20000, true), "closed")
check("fresh open lid still shows open", Model.caseIconState(false, Model.LID_OPEN, chargingPod, chargingPod, "open", false, 1000, 2000), "open")
check("disconnected stale in-ear report expires to closed", Model.caseIconState(false, Model.LID_OPEN, wornPod, chargingPod, "open-right", true, 1000, 30000, 20000, false), "closed")
check("one in ear keeps the other pod in the case without a docked flag", Model.caseIconState(false, Model.LID_OPEN, wornPod, restingPod, "open-right", true, 1000, 2000, 20000, true), "open-right")
check("the other ear keeps its pod in the case without a docked flag", Model.caseIconState(false, Model.LID_OPEN, restingPod, wornPod, "open-left", true, 1000, 2000, 20000, true), "open-left")
check("a bud still charging stays in the case", Model.caseIconState(false, Model.LID_OPEN, wornPod, chargingPod, "open", true, 1000, 2000, 20000, true), "open-right")
check("a docked bud that is not charging stays in the case", Model.caseIconState(false, Model.LID_OPEN, wornPod, { inEar: false, charging: false, inCase: true }), "open-right")
check("connected in-ear report survives BLE silence", Model.caseIconState(false, Model.LID_OPEN, wornPod, chargingPod, "open-right", true, 1000, 30000, 20000, true), "open-right")
check("disconnected fresh one-bud report stays open", Model.caseIconState(false, Model.LID_OPEN, wornPod, chargingPod, "open-right", true, 1000, 2000, 20000, false), "open-right")
check("an open lid still shows the other pod while disconnected", Model.caseIconState(false, Model.LID_OPEN, wornPod, restingPod, "open", true, 1000, 2000, 20000, false), "open-right")
check("one in-ear bit is not both worn", Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, restingPod, "open", true, 1000, 2000, 20000, true), "open-right")

check("two-minute silence expires both worn buds", Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, wornPod, "", true, 1000, 121000, 20000, false), "closed")
check("two-minute silence expires one worn bud without charging", Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, restingPod, "", true, 1000, 121000, 20000, false), "closed")
check("connected buds survive prolonged BLE silence", Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, wornPod, "", true, 1000, 301000, 20000, true), "")
check("fresh BLE restores the worn icon", Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, wornPod, "closed", true, 301000, 301001, 20000, false), "")

// Disconnect clears ear_reported and the pod flags before the next advertisement restores them.
{
  const cleared = Model.parseStatus(JSON.stringify({ schema_version: 1, connected: false, ear_reported: false,
    last_advertisement_ms: 1000, left: { in_ear: false }, right: { in_ear: false } }))
  check("a cleared ear report stays unknown", cleared.earReported, false)
  let icon = Model.caseIconState(false, Model.LID_UNKNOWN, cleared.left, cleared.right, "", true,
    cleared.lastAdvertisementMs, 2000, 20000, cleared.connected, cleared.earReported)
  check("yielding the seat keeps the worn pods image", icon, "")
  check("an immediate worn advertisement does not flash the empty case", Model.caseIconState(false,
    Model.LID_UNKNOWN, wornPod, wornPod, icon, true, 2500, 2501, 20000, false, true), "")
  icon = Model.caseIconState(false, Model.LID_UNKNOWN, cleared.left, cleared.right, icon, true,
    cleared.lastAdvertisementMs, 30000, 20000, false, cleared.earReported)
  check("waiting for an advertisement does not invent a case change", icon, "")
  icon = Model.caseIconState(false, Model.LID_UNKNOWN, wornPod, wornPod, icon, true,
    31000, 31001, 20000, false, true)
  check("the next worn advertisement does not replay the empty case", icon, "")
  check("a real one-bud report still changes the case image", Model.caseIconState(false, Model.LID_OPEN,
    wornPod, chargingPod, icon, true, 32000, 32001, 20000, false, true), "open-right")
  check("a real docked report still closes the case", Model.caseIconState(false, Model.LID_CLOSED,
    chargingPod, chargingPod, icon, true, 33000, 33001, 20000, false, true), "closed")
  check("unknown ear placement still expires when telemetry is unavailable", Model.caseIconState(false,
    Model.LID_UNKNOWN, cleared.left, cleared.right, icon, true, 1000, 121000, 20000, false, false), "closed")
}

check("fresh report is available", Model.telemetryUnavailable(false, 1000, 120999), false)
check("expiry begins at two minutes", Model.telemetryUnavailable(false, 1000, 121000), true)
check("a report that has not arrived is not the case", Model.telemetryUnavailable(false, 0, 121000), false)
check("live connection overrides BLE expiry", Model.telemetryUnavailable(true, 1000, 301000), false)
check("fresh report clears expiry", Model.telemetryUnavailable(false, 301000, 301001), false)
check("a quiet case does not keep the last charge", Model.displayedBatteryLevel(30, true, false, 1000, 121000), Model.LEVEL_UNKNOWN)
check("a live charge still draws", Model.displayedBatteryLevel(30, true, false, 1000, 20000), 30)
check("a connected laptop keeps its charge after the advertisement ages", Model.displayedBatteryLevel(38, true, true, 1000, 301000), 38)
check("an offline daemon draws no charge", Model.displayedBatteryLevel(30, false, false, 1000, 20000), Model.LEVEL_UNKNOWN)

check("passive connection does not log an unreported audio owner",
  domainCaption(false, [{ label: "MacBook Pro" }], [{ label: "Linux laptop" }], false, false, false, true),
  "")
check("explicit claim replaces the neutral caption",
  domainCaption(false, [], [{ label: "Linux laptop" }], false, false, true, true),
  "Linux laptop claims ownership.")

check("a BlueZ link without control setup does not claim a confirmed seat",
  domainCaption(false, [{ label: "iPhone" }], [{ label: "Linux laptop" }], false, true, false, false, true),
  "Bluetooth connected. Waiting for Linux laptop to finish AirPods setup.")
check("unknown disconnected owner does not imply a Gram connection",
  domainCaption(false, [{ label: "MacBook Pro" }], [{ label: "Linux laptop" }], false, true, false, true),
  "Linux laptop is not connected to the AirPods.")


for (const state of [4, 5, 6, 7, 9])
  check('fresh BLE connection state ' + state, Model.connectionElsewhere(false, state, 2000, 3000, 1000), true)
for (const state of [0, 255, -1, 8])
  check('unknown or disconnected BLE state ' + state, Model.connectionElsewhere(false, state, 2000, 3000, 1000), false)
check('local connection overrides BLE inference', Model.connectionElsewhere(true, 5, 2000, 3000, 1000), false)
check('this computer holding the link overrides BLE inference', Model.connectionElsewhere(false, 4, 2000, 3000, 1000, true), false)
check('stale BLE cannot infer another connection', Model.connectionElsewhere(false, 5, 2000, 22000, 1000), false)
check('pre-disconnect BLE cannot infer another connection', Model.connectionElsewhere(false, 5, 1000, 3000, 2000), false)
check('future BLE timestamp is invalid', Model.connectionElsewhere(false, 5, 4000, 3000, 1000), false)
check('missing pairing key reads as ready', Model.parseStatus('{"schema_version":1}').pairingKeysReady, true)
check('pairing keys not ready', Model.parseStatus('{"schema_version":1,"pairing_keys_ready":false}').pairingKeysReady, false)
check('pairing mode defaults to off', Model.parseStatus('{"schema_version":1}').pairingMode, false)
check('pairing mode comes from daemon evidence', Model.parseStatus('{"schema_version":1,"pairing_mode":true}').pairingMode, true)
check('pairing mode advances the setup instruction', Model.emptyGuidance(true, false, 11, true, true), 'Your AirPods are ready to pair. Select them in the Bluetooth plugin.')
check('learn sentence while the key is missing', Model.emptyGuidance(true, false), 'Open the case lid with both AirPods inside.')
check('away sentence once the key is stored', Model.emptyGuidance(true, true), 'Open the lid on the AirPods case to allow Bluetooth connections to the pods.')
check('stopped daemon sentence', Model.emptyGuidance(false, false), 'Start the FleetPods service to see the Apple and Omarchy seats.')
check('unknown model uses the dotted art', Model.iconVariant(false, false, 0), 'unknown')
check('pro model uses the pro art', Model.iconVariant(false, true, 11), 'pro')
check('known buds keep the outline', Model.iconVariant(false, false, 3), 'buds')
check('headset uses the max art', Model.iconVariant(true, false, 7), 'max')
check('missing model int keeps the outline', Model.iconVariant(false, false, -1), 'buds')
check('parsed unknown model int', Model.parseStatus('{"schema_version":1,"model_int":0}').modelInt, 0)
// Public placement reports select artwork without a pairing key or battery level.
{
  let state = 'closed'
  let history = null
  for (const [lid, leftEar, rightEar, docked, expected] of [
    [Model.LID_CLOSED, false, false, true, 'closed'],
    [Model.LID_OPEN, false, false, true, 'open'],
    [Model.LID_UNKNOWN, true, false, false, 'open-right'],
    [Model.LID_UNKNOWN, true, true, false, 'open-empty'],
    [Model.LID_UNKNOWN, true, true, false, ''],
    [Model.LID_OPEN, false, false, true, 'open'],
    [Model.LID_CLOSED, false, false, true, 'closed']
  ]) {
    const now = (history ? history.now : 30000) + 1000
    const status = Model.parseStatus(JSON.stringify({ schema_version: 1, model_int: 11,
      pairing_keys_ready: false, connected: false, ear_reported: true,
      lid_state: lid, last_known_lid_state: lid, lid_updated_ms: now, last_advertisement_ms: now,
      left: { available: true, in_ear: leftEar, in_case: docked },
      right: { available: true, in_ear: rightEar, in_case: docked }
    }))
    history = Model.statusLog(history, { t: now, ev: 'status', status })
    state = Model.caseIconState(false, history.view.iconLidState, status.left, status.right,
      state, false, now, now, 20000, false, status.earReported)
    check('unpaired placement artwork ' + expected, state, expected)
    check('unpaired placement leaves batteries unknown', [status.left.level, status.right.level], [-1, -1])
    check('placement preserves pairing guidance', history.guidanceVisible, true)
    // The empty-case animation finishes on a timer before the next icon update.
    if (state === 'open-empty') state = ''
  }
}
// Detecting nearby AirPods must not hide the first-run pairing instructions.
{
  const status = { ...Model.defaultStatus(), ok: true, pairingKeysReady: false,
    modelInt: 11, lastAdvertisementMs: 30000, bleConnectionState: 5,
    controlConnection: { stage: 'waiting' } }
  const log = Model.statusLog(null, { t: 30000, ev: 'status', status })
  check('nearby unpaired pods retain pairing guidance', log.guidanceVisible, true)
  check('nearby unpaired pods explain setup in the header', log.subtitle, 'Pair AirPods to get started')
  check('nearby unpaired pods do not produce connection log entries', log.rows, [])
  check('nearby Pro 3 explains the front gesture', Model.emptyGuidance(true, false, 11, true, false, 0, true).includes('Double-tap'), true)
  const connecting = Model.statusLog(log, { t: 31000, ev: 'status', status: {
    ...status, bluezConnected: true, controlConnection: { stage: 'handshake' }
  } })
  check('pairing guidance yields to the real connection', connecting.guidanceVisible, false)
}
// First-run setup hides fleet discovery until AirPods setup is complete.
{
  const forgotten = Model.parseStatus(JSON.stringify({ schema_version: 1,
    bluetooth_paired: false, pairing_keys_ready: true, pairing_mode: true,
    model_int: 11, connected: false, bluez_connected: false,
    control_connection: { stage: 'connecting' },
    devices: [{ address: '02:00:00:00:00:04', name: 'Gram', kind: 'omarchy', nearby: true }]
  }))
  check('saved keys survive Bluetooth forget', forgotten.pairingKeysReady, true)
  check('Bluetooth pairing is reported separately', forgotten.bluetoothPaired, false)
  const log = Model.statusLog(null, { t: 30000, ev: 'status', status: forgotten })
  check('Bluetooth forget restores setup despite cached keys and stale connecting', log.pairingRequired, true)
  check('forgotten device asks to pair', log.subtitle, 'Pair AirPods to get started')
  check('forgotten device shows setup guidance', log.guidanceVisible, true)
  check('forgotten device hides fleet trays', [log.seat.availableApple, log.seat.availableOmarchy], [[], []])
  check('forgotten device empties both seats', log.seat.seats, { apple: [], omarchy: [] })
  const paired = Model.parseStatus('{"schema_version":1,"bluetooth_paired":true,"pairing_keys_ready":true}')
  check('pairing again finishes setup', Model.statusLog(log, { t: 31000, ev: 'status', status: paired }).pairingRequired, false)
}

for (const paired of [true, false]) {
  const peer = '02:00:00:00:00:04'
  const log = Model.statusLog(null, { t: 30000, ev: 'status', status: {
    ...Model.defaultStatus(), ok: true, hasAirPods: paired, pairingKeysReady: paired,
    caseLidState: Model.LID_CLOSED, caseConfirmedDocked: true, lastAdvertisementMs: 0,
    controlConnection: { stage: 'waiting' },
    devices: { [peer]: { label: 'Nearby laptop', kind: 'omarchy', nearby: true, discovered: true } }
  } })
  check('fleet seats remain visible without available pods, paired=' + paired, log.showSeats, true)
  check('case guidance remains below the seats', log.guidanceVisible, true)
  check('only initialized setup exposes fleet discovery', log.seat.availableOmarchy.some(p => p.address === peer && p.discovered), paired)
  if (!paired) {
    check('first-run trays are empty', [log.seat.availableApple, log.seat.availableOmarchy], [[], []])
    check('first-run seats are empty', log.seat.seats, { apple: [], omarchy: [] })
    check('first-run seats have no connection actions', [log.seat.connectedToOmarchy, log.seat.omarchyTransitionLabel], [false, ''])
  }
  check('showing seats does not revive closed-case errors', log.rows, [])
  const gone = Model.statusLog(log, { t: 31000, ev: 'gone' })
  check('an offline daemon still shows its guidance', [gone.showSeats, gone.guidanceVisible], [false, true])
}
{
  const local = '02:00:00:00:00:01'
  const status = { ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
    localAddress: local, localName: 'laptop-a', lastAdvertisementMs: 1000,
    devices: { [local]: { label: 'laptop-a', kind: 'omarchy' } },
    controlConnection: { stage: 'ready' } }
  let log = Model.statusLog(null, { t: 1000, ev: 'status', status })
  const closing = Model.statusLog(log, { t: 1500, ev: 'status', status: { ...status,
    caseConfirmedDocked: true, caseLidState: Model.LID_CLOSED, lidState: Model.LID_CLOSED,
    lastAdvertisementMs: 1500 } })
  check('closed case overrides availability while Bluetooth finishes disconnecting',
    [closing.subtitle, closing.description, closing.guidanceVisible, closing.view.connected],
    ['Case closed', 'Case closed', false, true])
  check('closing the case keeps the normal status log until disconnect', closing.rows.length > 0, true)
  for (const [connected, bluezConnected] of [[false, true], [true, false]]) {
    const partial = Model.statusLog(closing, { t: 1750, ev: 'status', status: {
      ...closing.status, connected, bluezConnected,
      controlConnection: { stage: connected ? 'ready' : 'interrupted' }
    } })
    check('either remaining connection delays lid-opening guidance', partial.guidanceVisible, false)
  }
  log = Model.statusLog(closing, { t: 2000, ev: 'status', status: { ...status,
    connected: false, bluezConnected: false, caseConfirmedDocked: true,
    caseLidState: Model.LID_CLOSED, lidState: Model.LID_CLOSED, lastAdvertisementMs: 2000, controlConnection: { stage: 'waiting' } } })
  check('case closure removes the disconnected local icon immediately',
    [log.guidanceVisible, log.seat.seats.omarchy.map(d => d.address)], [true, []])
  check('case closure removes the local connected line', log.seat.connectedToOmarchy, false)
  check('a closed case does not invite a connection', log.seat.omarchyTransitionLabel, '')
  log = Model.statusLog(log, { t: 4999, ev: 'tick' })
  check('the disconnected local icon waits in the tray during case settling',
    log.seat.availableOmarchy.map(d => d.address), [local])
  log = Model.statusLog(log, { t: 5000, ev: 'tick' })
  check('case guidance and the local tray move start together',
    [log.guidanceVisible, log.seat.seats.omarchy, log.seat.availableOmarchy.map(d => d.address)], [true, [], [local]])
}
// Docking flags can expire independently while charging and the closed lid remain known.
for (const missingSide of ['left', 'right', 'both']) {
  const wire = { schema_version: 1, connected: false, bluez_connected: false,
    case_confirmed_docked: false, lid_state: 1, last_known_lid_state: 1, lid_updated_ms: 1000,
    control_connection: { stage: 'waiting' },
    devices: [{ address: '02:00:00:00:00:02', kind: 'apple', label: 'iPhone' }],
    left: { available: true, level: 61, charging: true, in_case: true, in_ear: false },
    right: { available: true, level: 62, charging: true, in_case: true, in_ear: false } }
  if (missingSide !== 'right') delete wire.left.in_case
  if (missingSide !== 'left') delete wire.right.in_case
  const shown = Model.statusLog(null, { t: 2000, ev: 'status', status: Model.parseStatus(JSON.stringify(wire)) })
  check('charging evidence keeps header and seat prompts consistent without ' + missingSide + ' docking flags',
    [shown.subtitle, shown.seat.omarchyTransitionLabel, shown.seat.appleSeatUnknown, shown.seat.appleTrayRemembered],
    ['Case closed', '', false, true])
  wire.right.in_ear = true
  wire.right.charging = false
  const worn = Model.statusLog(null, { t: 2000, ev: 'status', status: Model.parseStatus(JSON.stringify(wire)) })
  check('a worn bud contradicts closed-case guidance despite an old docked flag',
    [worn.subtitle === 'Case closed', worn.seat.omarchyTransitionLabel], [false, ''])
}
check('closed case screen clears the status log', Model.statusLogSuppressed(true, 'inCase'), true)
check('waiting for AirPods starts a fresh log session', Model.statusLogSuppressed(true, 'waiting'), true)
check('a connection retry keeps the current log session', Model.statusLogSuppressed(true, 'retrying'), false)
check('opening socket is a real connection attempt', Model.openingControlLink(false, {stage: 'connecting'}, {}), true)
check('fleet request is a real connection attempt', Model.openingControlLink(false, {stage: 'idle'}, {phase:'waiting_admission',busy:true}), true)
check('typing with idle control does not invent a connection attempt', Model.openingControlLink(false, {stage:'idle'}, {phase:'failed',busy:false}), false)
check('yielding helper is not connecting locally', Model.openingControlLink(false, {stage:'idle'}, {phase:'waiting_connection',busy:true}), false)
check('control link ends the dial', Model.openingControlLink(true, {stage:'ready'}, {}), false)
check('same caption is not a new log line', Model.statusCaptionChanged('Not connected to LG Gram.', 'Not connected to LG Gram.'), false)
check('a blank is not a new log line', Model.statusCaptionChanged('Not connected to LG Gram.', '  '), false)
check('a later caption is a new log line', Model.statusCaptionChanged('Not connected to LG Gram.', 'Connected to LG Gram.'), true)
check('in-case seats keep the status log', Model.statusLogSuppressed(false, 'inCase'), false)
check('open lid without the in-case header keeps the status log', Model.statusLogSuppressed(true, 'notConnected'), false)
check('missing BLE state defaults unknown', Model.parseStatus('{"schema_version":1}').bleConnectionState, 255)
check('BLE connection state parses', Model.parseStatus('{"schema_version":1,"ble_connection_state":5}').bleConnectionState, 5)

const rememberedStatus = {
  ...Model.defaultStatus(), connected: true, audioOwner: REMOTE_PLAYING,
  connectedPeers: ["02:00:00:00:00:02"], localAddress: "02:00:00:00:00:01",
  localName: "Laptop", remoteSourceAddress: "02:00:00:00:00:02",
  devices: { "02:00:00:00:00:02": { label: "Phone", kind: "apple" } }
}
const confirmedPresentation = Model.connectionPresentation(rememberedStatus, null, 1000)
// 2026-10-04: yielding the background slot blanked both seats until the peer's roster arrived.
const yieldingStatus = { ...rememberedStatus, connected: false, connectedPeers: [],
  fleet: { routing: { phase: "waiting_connection", busy: true, admission: "verify_connection", peer: "02:00:00:00:00:03" } },
  devices: { ...rememberedStatus.devices, "02:00:00:00:00:03": { label: "laptop-b", kind: "omarchy" } } }
const yieldingPresentation = Model.connectionPresentation(yieldingStatus, confirmedPresentation, 4000)
const yielding = yieldingPresentation.snapshot
check('yielding keeps the playing iPhone seated and connected', yielding && yielding.seats.apple.map(d => [d.label, d.onAcl]), [["Phone", true]])
check('yielding moves this laptop out of the seat', yielding && yielding.seats.omarchy.map(d => d.label), [])
check('yielding names the laptop taking the slot', yielding && yielding.peerLabel, "laptop-b")
check('yielding keeps the iPhone as the audio owner', yielding && [yielding.appleOwnsAudio, yielding.sourceAddress], [true, "02:00:00:00:00:02"])
check('a finished handover still holds until the peer roster lands', !!Model.connectionPresentation({ ...yieldingStatus,
  fleet: { routing: { phase: "success", busy: false } } }, yieldingPresentation, 6000).snapshot, true)
check('an idle disconnect does not invent a handover', Model.connectionPresentation({ ...yieldingStatus,
  fleet: { routing: { phase: "idle", busy: false } } }, confirmedPresentation, 4000).snapshot, null)
const remoteLaptopStatus = { ...rememberedStatus,
  devices: { "02:00:00:00:00:02": { label: "laptop-b", kind: "omarchy" } } }
check('fresh remote Omarchy source highlights its domain', Model.remoteOmarchyAudio(remoteLaptopStatus), true)
check('remote Omarchy does not imply local ownership', Model.linuxHeld(remoteLaptopStatus), false)
for (const [name, patch] of Object.entries({
  disconnected: { connected: false }, local: { audioOwner: LOCAL_PLAYING }, idle: { audioOwner: REMOTE },
  unreported: { audioOwner: UNKNOWN }, unknown: { remoteSourceAddress: '' },
  unrecognized: { devices: {} }, apple: { devices: rememberedStatus.devices }
})) check('remote Omarchy excludes ' + name, Model.remoteOmarchyAudio({ ...remoteLaptopStatus, ...patch }), false)
const remoteLaptopPresentation = Model.connectionPresentation(remoteLaptopStatus, null, 1000)
check('remote laptop retains the Omarchy domain', remoteLaptopPresentation.lastKnown?.linuxHeld, true)
check('remote laptop snapshot names the playing peer', remoteLaptopPresentation.lastKnown?.label, 'laptop-b')
const noReport = { ...rememberedStatus, audioOwner: UNKNOWN }
const refreshingPresentation = Model.connectionPresentation(noReport, confirmedPresentation, 2000)
check('unconfirmed connection retains last confirmed Apple seat', refreshingPresentation.snapshot?.linuxHeld, false)
check('refresh clearly marked as pending', refreshingPresentation.refreshing, true)
check('last device name survives refresh', refreshingPresentation.snapshot?.seats.apple[0].label, 'Phone')
check('refresh ends at five seconds', Model.connectionPresentation(noReport, refreshingPresentation, 7000).refreshing, false)
check('expired refresh discards optimistic display', Model.connectionPresentation(noReport, refreshingPresentation, 7000).snapshot, null)
const newOwner = { ...rememberedStatus, audioOwner: LOCAL_PLAYING }
const newPresentation = Model.connectionPresentation(newOwner, refreshingPresentation, 3000)
check('fresh source ends refresh immediately', newPresentation.refreshing, false)
check('fresh local owner updates remembered state', newPresentation.lastKnown?.linuxHeld, true)
check('connection failure ends refresh', Model.connectionPresentation({ ...noReport, connected: false }, refreshingPresentation, 3000).refreshing, false)
check('explicit claim overrides old seat', Model.connectionPresentation({ ...noReport, wantOmarchySeat: true }, refreshingPresentation, 3000).refreshing, false)
check('case docking overrides old seat', Model.connectionPresentation({ ...noReport, caseConfirmedDocked: true }, refreshingPresentation, 3000).snapshot, null)
check('cold start does not invent an owner', Model.connectionPresentation(noReport, null, 2000).snapshot, null)
check('stale remembered state is not shown', Model.connectionPresentation(noReport, confirmedPresentation, 122000).snapshot, null)

const emptyApplePresentation = Model.connectionPresentation({
  ...rememberedStatus, connectedPeers: [], audioOwner: LOCAL_PLAYING
}, null, 1000)
const joinedWhileOwnerUnknown = Model.connectionPresentation({
  ...noReport, peerListUpdated: 2100, controlConnection: { stage: "ready", peer_reconnect: false }
}, emptyApplePresentation, 2200)
check('fresh roster replaces disconnected seat before ownership arrives',
  joinedWhileOwnerUnknown.snapshot?.seats.apple[0]?.onAcl, true)
const absentWhileOwnerUnknown = Model.connectionPresentation({
  ...noReport, connectedPeers: [], peerListUpdated: 2100
}, confirmedPresentation, 2200)
check('fresh empty roster clears remembered connected Apple seat',
  absentWhileOwnerUnknown.snapshot?.seats.apple[0]?.onAcl, false)
check('old snapshot remains unchanged after fresh roster',
  confirmedPresentation.lastKnown.seats.apple[0].onAcl, true)

// Once this observer loses its link, the old Apple owner is a remembered
// device, not a confirmed seat or playing source, even during a retry.
for (const recovering of [false, true]) {
  const mac = '02:00:00:00:00:02', xps = '02:00:00:00:00:01'
  const playing = { ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
    localAddress: xps, localName: 'XPS', connectedPeers: [mac, xps], peerListUpdated: 1000,
    audioOwner: REMOTE_PLAYING, remoteSourceAddress: mac, rosterAudioOwner: mac,
    devices: { [mac]: { kind: 'apple', label: 'Mac' } },
    left: { inEar: true }, controlConnection: { stage: 'ready' } }
  let state = Model.statusLog(null, { ev: 'status', t: 1000, status: playing })
  const retry = { ...playing, connected: false, bluezConnected: false, connectedPeers: [],
    peerListUpdated: 0, audioOwner: UNKNOWN, remoteSourceAddress: '', rosterAudioOwner: '', devices: {},
    controlConnection: { stage: recovering ? 'retrying' : 'connecting', recovering } }
  state = Model.statusLog(state, { ev: 'status', t: 2000, status: retry })
  check('background retry leaves the Apple occupant unknown',
    [state.seat.seats.apple, state.seat.appleSeatUnknown], [[], true])
  check('background retry moves the remembered Mac into the tray',
    state.seat.availableApple.map(d => [d.address, d.label]), [[mac, 'Mac']])
  check('background retry cannot claim the old Mac is still playing',
    [state.seat.audioDeviceAddress, state.seat.appleOwnsAudio, state.seat.audioPlaying], ['', false, false])
  check('remembered devices do not enable Apple connection actions', state.seat.appleTrayAvailable, false)
  check('XPS leaves the connected seat during a retry', state.seat.seats.omarchy, [])
  check('XPS waits in the tray during a retry', state.seat.availableOmarchy.map(d => d.address), [xps])
  check('XPS retry does not draw a connected link', state.seat.connectedToOmarchy, false)
  check('the empty XPS seat names the pending attempt', state.seat.omarchyTransitionLabel,
    recovering ? 'Reconnecting…' : 'Connecting…')
  check('the attempt kind is typed beside its label', state.seat.attempt,
    { kind: recovering ? 'reconnecting' : 'connecting', label: recovering ? 'Reconnecting…' : 'Connecting…' })
  check('background retry header does not name a stale owner', state.subtitle,
    recovering ? 'Retrying connection' : 'Connecting')
  check('background retry schedules a wake before the playback carry expires',
    state.wakeAt > 0 && state.wakeAt <= 16000, true)
  state = Model.statusLog(state, { ev: 'tick', t: 16000 })
  check('expired playback carry no longer claims Playing in the header', state.subtitle.includes('Playing'), false)
  check('expired playback carry no longer animates the Mac', state.seat.audioPlaying, false)
  const setupWithoutRoster = Model.statusLog(state, { ev: 'status', t: 16500, status: {
    ...retry, devices: playing.devices, connected: true, bluezConnected: true,
    controlConnection: { stage: 'ready' }
  } })
  check('a reopened socket cannot reseat the old owner before a fresh roster',
    [setupWithoutRoster.seat.appleSeatUnknown, setupWithoutRoster.seat.seats.apple], [true, []])
  const returned = Model.statusLog(state, { ev: 'status', t: 17000, status: playing })
  check('fresh Mac telemetry restores its playing seat',
    [returned.seat.seats.apple[0]?.label, returned.seat.audioPlaying], ['Mac', true])
  check('XPS returns to its seat once confirmed', returned.seat.seats.omarchy.map(d => d.address), [xps])
  const dropped = Model.statusLog(returned, { ev: 'status', t: 18000, status: { ...retry,
    bluezConnected: true, connectedPeers: [mac, xps],
    controlConnection: { stage: 'interrupted', reason: 'link_lost', recovering: true } } })
  check('stale BlueZ and roster data cannot retain the disconnected XPS',
    [dropped.seat.seats.omarchy, dropped.seat.connectedToOmarchy, dropped.seat.omarchyTransitionLabel],
    [[], false, ''])
  check('an interrupted connection is not a live retry', dropped.rows.some(row => row.live), false)
  check('the interrupted header does not claim a connection attempt', dropped.subtitle, 'Not connected')
  check('the interrupted status explains what will reconnect', dropped.description,
    'Disconnected. Waiting for activity or Play.')
  check('stale BlueZ cannot label the interrupted laptop as connecting',
    Model.localConnectionLabel(dropped.view), 'Disconnected · waiting for activity or Play')
  const waiting = Model.statusLog(dropped, { ev: 'status', t: 19000, status: {
    ...retry, waitingForPlayback: true,
    controlConnection: { stage: 'interrupted', reason: 'link_lost', recovering: true }
  } })
  check('waiting for fresh input never animates a retry',
    [waiting.rows.some(row => row.live), waiting.view.connecting], [false, false])
  check('the latest log row explicitly ends the connection attempt',
    waiting.rows.at(-1).message, 'Disconnected. Waiting for activity or Play.')
  const advertised = Model.statusLog(waiting, { ev: 'status', t: 20000, status: {
    ...retry, lastAdvertisementMs: 19500, bleConnectionState: 4,
    controlConnection: { stage: 'interrupted', reason: 'link_lost', recovering: true }
  } })
  check('fresh BLE can confirm a connection elsewhere without identifying its owner',
    [advertised.subtitle, advertised.seat.appleSeatUnknown, advertised.seat.audioDeviceAddress],
    ['Connected elsewhere', true, ''])
}
check('roster timestamp parses', Model.parseStatus(
  '{"schema_version":1,"peer_list_updated_ms":2100}').peerListUpdated, 2100)

const idleReady = { connected: true, controlConnection: { stage: "ready" },
  nativeHandoff: { audio_output: "airpods", connecting: false, failure: "" },
  reportedOwnership: -1, sourceUpdated: 100, audioSourceLocal: 1, audioSourceType: 0,
  ownershipIntent: "none", left: { inEar: true }, right: {} }
check("idle local output uses the ready connector", Model.idleAudioReady(idleReady), true)
check("Apple ownership overrides idle readiness", Model.idleAudioReady({ ...idleReady, reportedOwnership: 0 }), false)
check("active phone overrides idle readiness", Model.idleAudioReady({ ...idleReady, audioSourceLocal: 0, audioSourceType: 2 }), false)
check("reconnect release intent does not hide confirmed idle readiness", Model.idleAudioReady({ ...idleReady, ownershipIntent: "release" }), true)
check("confirmed local ownership retains reconnect readiness", Model.idleAudioReady({ ...idleReady,
  ownershipIntent: "release", reportedOwnership: 1,
  nativeHandoff: { audio_output: "airpods", audio_active: false, connecting: false, failure: "playback_stopped" } }), true)
check("release without a fresh source report stays unconfirmed", Model.idleAudioReady({ ...idleReady, ownershipIntent: "release", sourceUpdated: 0 }), false)
check("reconnect cannot light the local seat during phone ownership", Model.idleAudioReady({ ...idleReady, ownershipIntent: "release", reportedOwnership: 0 }), false)
check("reconnect cannot light the local seat before the output exists", Model.idleAudioReady({ ...idleReady, ownershipIntent: "release", nativeHandoff: { audio_output: "" } }), false)
for (const reason of ["playback_stopped", "playback_paused"]) {
  check("reconnect readiness survives " + reason, Model.idleAudioReady({ ...idleReady,
    ownershipIntent: "release", nativeHandoff: { audio_output: "airpods", audio_active: false,
      audio_state: "remote", connecting: false, yielded: true, failure: reason } }), true)
}
check("routing failure still blocks readiness", Model.idleAudioReady({ ...idleReady,
  nativeHandoff: { audio_output: "airpods", failure: "audio_operation_timeout:routing" } }), false)
check("speaker default is not AirPods readiness", Model.idleAudioReady({ ...idleReady, nativeHandoff: { audio_output: "speakers" } }), false)
check("unfinished preparation keeps its animation", Model.idleAudioReady({ ...idleReady, nativeHandoff: { audio_output: "airpods", connecting: true } }), false)
check("docked buds do not imply playback readiness", Model.idleAudioReady({ ...idleReady, left: { inCase: true }, right: { inCase: true } }), false)
check("lost connection cannot retain ready connector", Model.idleAudioReady({ ...idleReady, connected: false }), false)

check("ownership alone does not imply playback", Model.localAudioLabel("profile_missing", ""), "Preparing audio")
check("saved speaker route stays pending", Model.localAudioLabel("routing", ""), "Routing audio")
check("failed routing is visible", Model.localAudioLabel("routing", "audio_operation_timeout:routing"), "Audio needs attention")
check("the held label names this laptop", Model.localAudioLabel("", "", true, false, "omarchy-xps"), "Omarchy fleet · omarchy-xps")
check("playback names this laptop", Model.localAudioLabel("playing", "", true, false, "omarchy-xps"), "Omarchy fleet · omarchy-xps · Playing")
check("idle output is ready", Model.localAudioLabel("ready", ""), "Omarchy fleet")
check("unworn buds are connected without an audio warning", Model.localAudioLabel("unavailable", "", false), "Connected")
check("worn buds retain unavailable audio warning", Model.localAudioLabel("unavailable", "", true), "Audio unavailable")
check("confirmed idle owner does not need an active audio output",
  Model.localAudioLabel("unavailable", "", true, false, "omarchy-xps", true), "Omarchy fleet · omarchy-xps")
check("idle ownership does not conceal an actual audio failure",
  Model.localAudioLabel("unavailable", "audio_route_timeout", true, false, "omarchy-xps", true), "Audio needs attention")
{
  const local = "A8:72:4D:0C:7C:EE", gram = "D0:C6:37:2F:D0:9F"
  const wire = { schema_version: 1, connected: true, bluetooth_paired: true, pairing_keys_ready: true,
    local_name: "omarchy-xps", local_address: local, connected_peers: [local, gram], peer_list_updated_ms: 1000,
    control_connection: { stage: "ready" }, reported_ownership: 0,
    audio_owner: { holder: "remote", playing: false }, remote_source_address: gram, roster_audio_owner: gram,
    audio_source_local: 0, audio_source_type: 0, source_updated_ms: 1000,
    devices: [{ address: gram, label: "lg-gram", kind: "omarchy", discovered: false }],
    right: { available: true, in_ear: true, in_case: false },
    native_handoff: { enabled: true, audio_state: "unavailable", phase: "idle", playing: false, failure: "" } }
  let log = Model.statusLog(null, { t: 1000, ev: "status", status: Model.parseStatus(JSON.stringify(wire)) })
  check("idle remote owner names Gram", log.subtitle, "Omarchy fleet · lg-gram")
  wire.reported_ownership = 1
  wire.audio_owner.holder = "local"
  wire.roster_audio_owner = local
  wire.ownership_updated_ms = 1100
  log = Model.statusLog(log, { t: 1100, ev: "status", status: Model.parseStatus(JSON.stringify(wire)) })
  check("idle ownership transfer names XPS without an unavailable warning", log.subtitle, "Omarchy fleet · omarchy-xps")
  check("idle transfer keeps both laptops seated", log.seat.seats.omarchy.map(d => d.address).sort(), [local, gram].sort())
  // Source and output updates arrive separately while ownership moves away.
  wire.audio_owner.playing = true
  wire.audio_source_type = 2
  wire.audio_source_local = 1
  wire.source_updated_ms = 1200
  log = Model.statusLog(log, { t: 1200, ev: "status", status: Model.parseStatus(JSON.stringify(wire)) })
  check("an unavailable local output cannot replace the known fleet owner during handoff",
    log.subtitle, log.seat.audioSubtitle)
  check("the handoff header still names XPS until Gram owns the audio",
    log.subtitle.startsWith("Omarchy fleet · omarchy-xps"), true)
  wire.reported_ownership = 0
  wire.audio_owner = { holder: "remote", playing: false }
  wire.roster_audio_owner = gram
  wire.audio_source_local = 0
  wire.ownership_updated_ms = 1300
  log = Model.statusLog(log, { t: 1300, ev: "status", status: Model.parseStatus(JSON.stringify(wire)) })
  check("handoff finishes directly on Gram's fleet label", log.subtitle, "Omarchy fleet · lg-gram")
  wire.reported_ownership = 1
  wire.audio_owner = { holder: "local", playing: false }
  wire.roster_audio_owner = local
  wire.native_handoff.failure = "audio_route_timeout"
  log = Model.statusLog(log, { t: 1400, ev: "status", status: Model.parseStatus(JSON.stringify(wire)) })
  check("a real audio failure still appears with a known fleet owner", log.subtitle, "Audio needs attention")
}
check("unworn buds retain real failures", Model.localAudioLabel("unavailable", "routing_failed", false), "Audio needs attention")
check("verified playback is shown", Model.localAudioLabel("playing", ""), "Omarchy fleet · Playing")
// 2026-10-07: shown only while the pods name this laptop the owner, a leftover "remote" read "Audio released".
check("a leftover release still names this laptop's fleet", Model.localAudioLabel("remote", "", true, false, "omarchy-xps"),
  "Omarchy fleet · omarchy-xps")
check("stopped playback is not an audio error", Model.localAudioLabel("remote", "playback_stopped"), "Omarchy fleet")
check("paused playback is not an audio error", Model.localAudioLabel("remote", "playback_paused"), "Omarchy fleet")
for (const failure of ["ownership_changed", "playback_stopped", "playback_paused"]) {
  check("confirmed playback heading survives " + failure,
    Model.localAudioLabel("remote", failure, true, true), "Omarchy fleet · Playing")
}
check("confirmed source does not hide a routing failure",
  Model.localAudioLabel("remote", "audio_operation_timeout:routing", true, true), "Audio needs attention")


const speakers = { localName: "Linux laptop", left: { inCase: true }, right: { inCase: true }, nativeHandoff: { audio_output: "speakers", audio_active: true, audio_state: "unavailable" } }
const podsOutput = { localName: "Linux laptop", left: { inEar: true }, right: {}, nativeHandoff: { audio_output: "airpods", audio_active: true, audio_state: "playing" } }
check("startup on speakers stays quiet", Model.audioRouteState(null, speakers).message, "")
check("startup on AirPods stays quiet", Model.audioRouteState(null, podsOutput).message, "")
let route = Model.audioRouteState(null, speakers)
route = Model.audioRouteState(route, { left: {}, right: {}, nativeHandoff: { audio_output: "speakers", audio_state: "routing" } })
check("preparing audio does not announce a switch", route.message, "")
route = Model.audioRouteState(route, podsOutput)
check("confirmed AirPods route announces the change", route.message, "AirPods audio moved to Linux laptop.")
route = Model.audioRouteState(route, { left: {}, right: {}, nativeHandoff: { audio_output: "", audio_state: "unavailable" } })
check("unknown output preserves last confirmed route", route.output, "airpods")
route = Model.audioRouteState(route, podsOutput)
check("same route after interruption does not add a change", route.message, "AirPods audio moved to Linux laptop.")
route = Model.audioRouteState(route, speakers)
// 2026-10-07: playing media takes the audio back from the speakers, so landing on them is not news.
check("a move to the speakers is not logged", route.message, "AirPods audio moved to Linux laptop.")
route = Model.audioRouteState(route, { left: {}, right: {}, nativeHandoff: { audio_output: "other", audio_active: true } })
check("another output holds nothing either", [route.message, route.audible], ["AirPods audio moved to Linux laptop.", "local:airpods"])
route = Model.audioRouteState(route, podsOutput)
check("returning from another output is not another take", route.audible, "local:airpods")
check('the connecting line cycles its dots', [1, 2, 3].map(d => Model.connectingLine('', d, false)),
  ['Connecting to AirPods.', 'Connecting to AirPods..', 'Connecting to AirPods...'])
check('a retry keeps its place while the dots cycle', [1, 3].map(d => Model.connectingLine('(Retrying 1 of 2)', d, false)),
  ['Connecting to AirPods.   (Retrying 1 of 2)', 'Connecting to AirPods... (Retrying 1 of 2)'])
check('a finished attempt says connected', Model.connectingLine('(Retrying 1 of 2)', 1, true), 'Connecting to AirPods... Connected!')
check('a first attempt has no retry', Model.connectionAttempt({ stage: 'handshake', retry: 1, retry_limit: 2 }), { detail: '' })
check('a recovering attempt names its retry', Model.connectionAttempt({ stage: 'connecting', recovering: true, retry: 1, retry_limit: 2 }),
  { detail: '(Retrying 1 of 2)' })
check('ready, waiting and failed are not attempts', ['ready', 'waiting', 'failed', 'idle'].map(stage => Model.connectionAttempt({ stage })),
  [null, null, null, null])
check('an Apple reconnect is not an AirPods attempt', Model.connectionAttempt({ stage: 'connecting', peer_reconnect: true }), null)
check('a control retry with the pods docked is not an attempt',
  Model.connectionAttempt(Model.caseQuietConnection({ stage: 'retrying', recovering: true, retry: 1, retry_limit: 2 }, true)), null)
check('a retry once the case opens is one again',
  Model.connectionAttempt(Model.caseQuietConnection({ stage: 'retrying', recovering: true }, false)), { detail: '(Retrying)' })
check('a first connect in the case still shows', Model.caseQuietConnection({ stage: 'handshake' }, true).stage, 'handshake')
check("missing route telemetry stays quiet", Model.audioRouteState(null, { left: {}, right: {}, nativeHandoff: { audio_state: "playing" } }).message, "")

let idleRoute = Model.audioRouteState(null, { ...speakers, nativeHandoff: { audio_output: "speakers", audio_active: false } })
for (const output of ["airpods", "speakers", "airpods"]) {
  idleRoute = Model.audioRouteState(idleRoute, { ...speakers, nativeHandoff: { audio_output: output, audio_active: false } })
  check("idle route change stays quiet: " + output, idleRoute.message, "")
}
const phonePlayback = { ...speakers, connected: true, audioOwner: REMOTE_PLAYING, remoteSourceAddress: "AA:AA:AA:AA:AA:01", devices: PEERS,
  nativeHandoff: { audio_output: "speakers", audio_active: false } }
// 2026-10-07 20:05: after a daemon restart the idle speaker guess turned the phone's ongoing playback into a "move".
check("the first device seen playing after an idle start is not a move",
  Model.audioRouteState(idleRoute, phonePlayback).message, "")
idleRoute = Model.audioRouteState(Model.audioRouteState(idleRoute, podsOutput), phonePlayback)
check("phone takeover names the audible device", idleRoute.message, "AirPods audio moved to iPhone.")
const otherLaptop = Model.audioRouteState(Model.audioRouteState(null, podsOutput), { ...phonePlayback,
  localAddress: "02:00:00:00:00:09", remoteSourceAddress: "BB:BB:BB:BB:BB:01" })
check("a laptop taking audio is a move within the Omarchy fleet", otherLaptop.message, "AirPods audio moved to Linux laptop.")
for (const output of ["airpods", "speakers", "airpods"]) {
  idleRoute = Model.audioRouteState(idleRoute, { ...phonePlayback, nativeHandoff: { audio_output: output, audio_active: false } })
  check("idle local routes preserve one phone takeover", idleRoute.message, "AirPods audio moved to iPhone.")
}
check("startup during phone playback is quiet", Model.audioRouteState(null, phonePlayback).message, "")
idleRoute = Model.audioRouteState(idleRoute, podsOutput)
check("actual local playback after phone names the laptop", idleRoute.message, "AirPods audio moved to Linux laptop.")
// 2026-10-07 20:22:47: releasing to the phone rerouted the stream airpods, speakers, airpods within 20 ms; the laptop
// still held the AirPods throughout, so the audio moved once.
let bounce = idleRoute
for (const output of ["speakers", "airpods", "speakers"])
  bounce = Model.audioRouteState(bounce, { ...podsOutput, nativeHandoff: { audio_output: output, audio_active: true, audio_state: "remote" } })
check("a reroute bounce while holding the AirPods is not another take", bounce.message, idleRoute.message)
const pausedLocal = { ...podsOutput, nativeHandoff: { audio_output: "airpods", audio_active: false } }
const pausedRoute = Model.audioRouteState(idleRoute, pausedLocal)
check("pause and resume on same output are quiet", Model.audioRouteState(pausedRoute, podsOutput).message, idleRoute.message)

const phoneRoute = Model.audioRouteState(null, phonePlayback)
const confirmedLocal = { ...phonePlayback, ownershipIntent: "release", audioOwner: LOCAL_PLAYING,
  connectedPeers: ["AA:AA:AA:AA:AA:01"],
  nativeHandoff: { audio_output: "airpods", audio_active: true, audio_state: "remote", failure: "ownership_changed" } }
check("fresh playback overrides a stale release for presentation", Model.confirmedLocalPlayback(confirmedLocal), true)
check("an owner decided by the ownership bit blocks local playback styling", Model.confirmedLocalPlayback({ ...confirmedLocal, audioOwner: LOCAL }), false)
check("phone playback cannot highlight the local connector", Model.confirmedLocalPlayback(phonePlayback), false)
check("an idle AirPods default cannot highlight playback", Model.confirmedLocalPlayback({ ...confirmedLocal,
  nativeHandoff: { ...confirmedLocal.nativeHandoff, audio_active: false } }), false)
check("new local source supersedes stale native remote summary",
  Model.audioRouteState(phoneRoute, confirmedLocal).message,
  "AirPods audio moved to Linux laptop.")
check("local ownership without a playing source stays quiet",
  Model.audioRouteState(phoneRoute, { ...confirmedLocal, audioOwner: LOCAL }).message, "")
check("local source without active media stays quiet",
  Model.audioRouteState(phoneRoute, { ...confirmedLocal,
    nativeHandoff: { ...confirmedLocal.nativeHandoff, audio_active: false } }).message, "")
check("disconnected source report cannot announce playback",
  Model.audioRouteState(phoneRoute, { ...confirmedLocal, connected: false }).message, "")

check("closed case waits instead of connecting", Model.controlConnectionLabel({ stage: "waiting" }),
  "Waiting for the case to open or a bud to be removed.")
check("socket opening is not connected", Model.controlConnectionLabel({ stage: "connecting" }), "Connecting to AirPods.")
check("socket established still awaits setup", Model.controlConnectionLabel({ stage: "handshake" }), "Connecting to AirPods.")
for (const stage of ["retrying", "connecting", "handshake", "features", "notifications"]) {
  check("one recovery message through " + stage,
    Model.controlConnectionLabel({ stage, recovering: true, retry: 1, retry_limit: 2 }),
    "Retrying connection (1 of 2)...")
}
check("ready restores normal ownership description", Model.controlConnectionLabel({ stage: "ready", recovering: true }), "")
check("exhausted recovery tells the reason", Model.controlConnectionLabel({ stage: "failed", reason: "Connection reset by peer" }),
  "AirPods connection unavailable (Connection reset by peer). Waiting for desktop activity.")
check("legacy daemon has no setup override", Model.controlConnectionLabel(null), "")
const retryStatus = Model.parseStatus(JSON.stringify({ schema_version: 1,
  control_connection: { stage: "retrying", recovering: true, retry: 2, retry_limit: 2 } }))
check("daemon retry report reaches the display", Model.controlConnectionLabel(retryStatus.controlConnection),
  "Retrying connection (2 of 2)...")

const fleetObserver = { connected: false, localAddress: "02:00:00:00:00:03", localName: "laptop-c",
  devices: { "02:00:00:00:00:01": { label: "laptop-a", kind: "omarchy" },
    "02:00:00:00:00:02": { label: "Phone", kind: "apple" },
    "02:00:00:00:00:03": { label: "laptop-c", kind: "omarchy", nearby: true } },
  fleet: { roster: { reporter: "02:00:00:00:00:01", peers: ["02:00:00:00:00:01", "02:00:00:00:00:02"],
    audio_source: "02:00:00:00:00:02" } } }
// A passive observer also stops showing a switch when its remembered roster
// expires. `recovering` on an interrupted link is not a running connection.
{
  const remembered = Model.connectionPresentation(fleetObserver, null, 1000)
  const disconnected = { ...fleetObserver, fleet: { routing: { phase: 'success', busy: false } },
    controlConnection: { stage: 'interrupted', recovering: true } }
  const view = Model.connectionPresentation(disconnected, remembered, 2000)
  check('interrupted control does not start a provisional fleet join', [view.refreshing, view.snapshot], [false, null])
}
const fleetView = Model.fleetSeatView(fleetObserver)
check('fleet observer seats the connected laptop', fleetView?.seats.omarchy.map(x => x.label), ['laptop-a'])
check('fleet observer seats the connected phone', fleetView?.seats.apple.map(x => x.label), ['Phone'])
check('disconnected observer waits above the seat', fleetView?.availableOmarchy.map(x => x.label), ['laptop-c'])
check('fleet phone audio highlights Apple', fleetView?.appleOwnsAudio, true)
check('local control reports override fleet views', Model.fleetSeatView({ ...fleetObserver, connected: true }), null)
check('expired fleet data has no view', Model.fleetSeatView({ ...fleetObserver, fleet: {} }), null)
const fleetLaptopView = Model.fleetSeatView({ ...fleetObserver,
  fleet: { roster: { ...fleetObserver.fleet.roster, audio_source: '02:00:00:00:00:01' } } })
check('fleet laptop audio highlights Omarchy', fleetLaptopView.omarchyOwnsAudio, true)
check('fleet laptop audio does not highlight Apple', fleetLaptopView.appleOwnsAudio, false)
check('fleet reports cannot contradict local disconnection', Model.fleetSeatView({ ...fleetObserver,
  fleet: { roster: { ...fleetObserver.fleet.roster, peers: ['02:00:00:00:00:01', '02:00:00:00:00:03'] } } }), null)
const fleetSilentView = Model.fleetSeatView({ ...fleetObserver,
  fleet: { roster: { ...fleetObserver.fleet.roster, audio_source: '' } } })
check('unknown fleet audio keeps both domains unclaimed', [fleetSilentView.appleOwnsAudio, fleetSilentView.omarchyOwnsAudio], [false, false])


// 2026-10-06: the helper only left the seat after its 2 s disconnect; it accepts within a second unless it plays.
const requestRouting = { phase: 'waiting_admission', busy: true, peer: '02:00:00:00:00:01' }
const requesting = { ...Model.defaultStatus(), ...fleetObserver, fleet: { ...fleetObserver.fleet, routing: requestRouting } }
check('a requested helper is no longer drawn from its roster', Model.fleetSeatView(requesting), null)
const beforeRequest = Model.connectionPresentation({ ...Model.defaultStatus(), ...fleetObserver }, null, 1000)
const duringRequest = Model.connectionPresentation(requesting, beforeRequest, 1200)
check('a requested helper leaves the seat at once',
  [duringRequest.snapshot?.seats.omarchy.map(p => p.label), duringRequest.snapshot?.seats.apple.map(p => p.label),
    duringRequest.snapshot?.localWaiting], [[], ['Phone'], true])
const playingHelper = { ...requesting, fleet: { roster: { ...fleetObserver.fleet.roster, audio_source: '02:00:00:00:00:01' },
  routing: requestRouting } }
check('a playing helper stays seated while it decides', Model.fleetSeatView(playingHelper)?.seats.omarchy.map(p => p.label), ['laptop-a'])
check('a refused request seats the helper again', Model.fleetSeatView({ ...requesting,
  fleet: { ...fleetObserver.fleet, routing: { ...requestRouting, phase: 'failed', busy: false } } })?.seats.omarchy.map(p => p.label), ['laptop-a'])

// Only a confirmed control session seats this laptop; a retry never retains it.
for (const stage of ['interrupted', 'idle', 'connecting', 'handshake', 'retrying', 'failed']) {
  check('an unconfirmed ' + stage + ' link does not seat XPS', Model.localSeatConnected({
    connected: stage === 'handshake', bluezConnected: true, controlConnection: { stage }
  }), false)
}
check('a ready control link seats XPS', Model.localSeatConnected({ connected: true,
  controlConnection: { stage: 'ready' } }), true)
check('legacy confirmed links still seat XPS', Model.localSeatConnected({ connected: true }), true)

// 2026-10-07: "Apple-held" named a vendor; the pods switch between two fleets, and the member is the useful fact.
check('a fleet names its audio member', Model.fleetLabel('Apple', 'iPhone'), 'Apple fleet · iPhone')
check('an unknown member leaves the fleet alone', Model.fleetLabel('Apple', ''), 'Apple fleet')
check('playback names the device', Model.playingLabel('omarchy-xps', 'Omarchy'), 'Omarchy fleet · omarchy-xps · Playing')
check('playback without a device names the fleet', Model.playingLabel('', 'Apple'), 'Apple fleet · Playing')
const appleIdle = { connected: true, localAddress: '02:00:00:00:00:03', rosterAudioOwner: '02:00:00:00:00:02',
  devices: { '02:00:00:00:00:02': { label: 'iPhone', kind: 'apple' } } }
check('the roster names the idle Apple member', Model.appleFleetMember(appleIdle), 'iPhone')
check('an Apple source names the playing member', Model.appleFleetMember({ ...appleIdle, rosterAudioOwner: '',
  audioOwner: REMOTE_PLAYING, audioSourceType: 2, remoteSourceAddress: '02:00:00:00:00:02' }), 'iPhone')
check('a laptop source is not an Apple member', Model.appleFleetMember({ ...appleIdle, rosterAudioOwner: '',
  remoteSourceAddress: '02:00:00:00:00:01', devices: { '02:00:00:00:00:01': { label: 'gram', kind: 'omarchy' } } }), '')

// 2026-10-07: the iPhone was playing while the header read "Apple fleet · iPhone".
check('an idle Apple member is named with its fleet', Model.appleFleetSubtitle(appleIdle), 'Apple fleet · iPhone')
check('a playing Apple member reads as playing', Model.appleFleetSubtitle({ ...appleIdle, rosterAudioOwner: '',
  audioOwner: REMOTE_PLAYING, remoteSourceAddress: '02:00:00:00:00:02' }), 'Apple fleet · iPhone · Playing')

// 2026-10-07: a laptop off the pods read "Connected nearby" while the iPhone held idle audio.
const idleFleet = Model.fleetSeatView({ ...fleetObserver, fleet: { roster: { ...fleetObserver.fleet.roster, audio_idle: true } } })
check('a broadcast idle owner names its fleet', Model.fleetViewSubtitle(idleFleet), 'Apple fleet · Phone')
check('a broadcast playing source adds Playing', Model.fleetViewSubtitle(Model.fleetSeatView(fleetObserver)), 'Apple fleet · Phone · Playing')
check('a broadcast without a source is nearby', Model.fleetViewSubtitle(Model.fleetSeatView({ ...fleetObserver,
  fleet: { roster: { ...fleetObserver.fleet.roster, audio_source: '' } } })), 'Connected nearby')

// The lit device pulses only while its owner plays; the other laptop learns that from the broadcast.
const ownerOf = (status, fleet, previous, carried) => Model.seatAudioOwner({ ...Model.defaultStatus(), ...status }, fleet, previous, carried)
// After reconnect, the iPhone reported streaming before the audio-source report changed from idle.
const streamingPhone = { ...Model.defaultStatus(), ...appleIdle, daemonReachable: true,
  remotePlaying: true, audioSourceLocal: 1, audioSourceType: 0 }
check('the remote playback flag is parsed', Model.parseStatus(JSON.stringify({ schema_version: 1, remote_playing: true })).remotePlaying, true)
check('remote streaming lights the identified iPhone owner', ownerOf(streamingPhone).playing, true)
const streamingSeat = Model.seatPresentation(streamingPhone, false, false)
check('remote streaming appears in the seat subtitle', streamingSeat.audioSubtitle, 'Apple fleet · iPhone · Playing')
check('a paused remote owner stops playing', ownerOf({ ...streamingPhone, remotePlaying: false }).playing, false)
check('remote streaming cannot light an unknown owner', ownerOf({ ...streamingPhone, rosterAudioOwner: '' }).playing, false)
check('remote streaming cannot light the local owner', ownerOf({ ...streamingPhone, rosterAudioOwner: streamingPhone.localAddress }).playing, false)
check('disconnected streaming cannot light the old owner', ownerOf({ ...streamingPhone, connected: false }).playing, false)
check('a playing owner pulses', ownerOf({ connected: true, audioOwner: REMOTE_PLAYING }, null).playing, true)
check('an idle owner holds a steady glow', ownerOf({ connected: true, audioOwner: REMOTE }, null).playing, false)
check('a broadcast playing source pulses', ownerOf({}, { sourcePlaying: true, appleOwnsAudio: true, sourceAddress: '02:00:00:00:00:02' }).playing, true)
check('a broadcast idle owner holds steady', ownerOf({}, { sourcePlaying: false, appleOwnsAudio: true, sourceAddress: '02:00:00:00:00:02' }).playing, false)
{
  const docked = { caseLidState: Model.LID_OPEN,
    left: { inCase: true, inEar: false }, right: { inCase: true, inEar: false } }
  const address = '02:00:00:00:00:01'
  const remembered = { appleOwnsAudio: false, sourceAddress: address, playing: true }
  const fleet = { sourcePlaying: true, appleOwnsAudio: false, sourceAddress: address }
  check('open-case docking quiets the live owner', ownerOf({ ...docked, connected: true, audioOwner: LOCAL_PLAYING }).playing, false)
  check('open-case docking quiets a fleet snapshot', ownerOf(docked, fleet).playing, false)
  check('open-case docking quiets a handover snapshot', ownerOf(docked, null, { ...remembered, handover: true }).playing, false)
  check('open-case docking quiets a carried owner', ownerOf(docked, null, null, remembered).playing, false)
  check('docking retains the carried device identity', ownerOf(docked, null, null, remembered).address, address)
  check('one worn bud can still play', ownerOf({ ...docked, right: { inCase: false, inEar: true } }, fleet).playing, true)
  const remote = { ...docked, daemonReachable: true, localAddress: '02:00:00:00:00:03',
    fleet: { roster: { reporter: address, peers: [address], audio_source: address, audio_idle: false } },
    devices: { [address]: { label: 'XPS', kind: 'omarchy' } } }
  const view = Model.seatPresentation(remote, false, false)
  check('docked fleet display clears the playing animation', view.audioPlaying, false)
  check('docked fleet display clears the playing subtitle', view.audioSubtitle, 'Omarchy fleet · XPS')
}
// 2026-10-07: a pending fleet view named no Omarchy owner while the subtitle, from the carried owner, named lg-gram.
check('a pending fleet view is no owner source for either fleet',
  ownerOf({}, { pending: true, omarchyOwnsAudio: false, appleOwnsAudio: false }, null,
    { appleOwnsAudio: false, sourceAddress: '02:00:00:00:00:01', playing: true }),
  { from: 'carried', apple: false, omarchy: true, address: '02:00:00:00:00:01', playing: true })

// 2026-10-07: a laptop taking the Omarchy seat flashed "Connecting"; a background join reads as the fleet switching.
const joinStage = (stage, extra) => ({ connected: false, ownershipIntent: 'none', controlConnection: { stage: stage, ...extra } })
check('a background join reads as switching', Model.backgroundJoin(joinStage('handshake')), true)
check('a background join retry still reads as switching', Model.backgroundJoin(joinStage('retrying')), true)
check('a claim keeps its own connecting state', Model.backgroundJoin({ ...joinStage('connecting'), ownershipIntent: 'claim' }), false)
check('a recovering link is not a join', Model.backgroundJoin(joinStage('retrying', { recovering: true })), false)
check('a ready link is not joining', Model.backgroundJoin(joinStage('ready')), false)
check('a fleet route in flight reads as switching', Model.backgroundJoin({ ...joinStage('idle'),
  fleet: { routing: { phase: 'connecting', busy: true } } }), true)

// 2026-10-07: "Omarchy fleet · Switching" hid the audio status for seconds; the seat shows the switch instead.
check('a fleet label carries its kind from its branch', Model.localAudio('ready', '').fleet, true)
check('a plain audio state is not a fleet label', Model.localAudio('routing', '').fleet, false)
check('a failure is not a fleet label', Model.localAudio('routing', 'audio_operation_timeout:routing').fleet, false)
check('a switch keeps the last audio status', Model.heldSubtitle(true, 'Not connected', 'Apple fleet · iPhone · Playing'),
  'Apple fleet · iPhone · Playing')
check('a switch without a known status shows the live one', Model.heldSubtitle(true, 'Connecting', ''), 'Connecting')
check('outside a switch the live status shows', Model.heldSubtitle(false, 'Omarchy fleet · lg-gram', 'Apple fleet · iPhone'),
  'Omarchy fleet · lg-gram')

// 2026-10-07: the seats lit Omarchy while the subtitle read "Apple fleet · iPhone"; both now follow the lit seat.
check('a lit Omarchy seat names its device', Model.seatAudioSubtitle(true, false, 'omarchy-xps', 'omarchy', false), 'Omarchy fleet · omarchy-xps')
check('a lit Apple seat names its device', Model.seatAudioSubtitle(false, true, 'iPhone', 'apple', true), 'Apple fleet · iPhone · Playing')
check('both lit defers to the lit device', Model.seatAudioSubtitle(true, true, 'omarchy-xps', 'omarchy', false), 'Omarchy fleet · omarchy-xps')
check('a device from the other fleet is not named', Model.seatAudioSubtitle(true, false, 'iPhone', 'apple', false), 'Omarchy fleet')
check('no lit seat leaves the fallback', Model.seatAudioSubtitle(false, false, '', '', false), '')

const localLinkStatus = { ...rememberedStatus, localName: 'laptop-a' }
// 2026-10-07: the line flipped between "connected" and "connected in background" as the phone played and paused.
// 2026-10-07: steady states belong to the subtitle and the seats; the line reports only what this laptop is doing.
check('a laptop beside the playing Apple fleet leaves the line empty', Model.localConnectionLabel(localLinkStatus), '')
check('a pause on the phone leaves the line empty', Model.localConnectionLabel({ ...localLinkStatus,
  audioOwner: REMOTE }), '')
check('a laptop holding the audio leaves the line empty', Model.localConnectionLabel({ ...localLinkStatus, audioOwner: LOCAL }), '')
check('a Bluetooth link before ownership is steady', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, bluezConnected: true }), '')
check('recovery is not a daemon outage', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, controlConnection: { stage: 'retrying', recovering: true } }), 'This laptop is reconnecting')
// 2026-10-07: a closed case read as "not connected" or "reconnecting" in the title.
check('docked pods read as in case', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, caseConfirmedDocked: true, controlConnection: { stage: 'waiting' } }), 'AirPods in case')
check('a retry is not shown while the pods are docked', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, caseConfirmedDocked: true, controlConnection: { stage: 'retrying', recovering: true } }), 'AirPods in case')
check('a live link with the lid closed is steady', Model.localConnectionLabel({ ...localLinkStatus,
  caseConfirmedDocked: true }), '')
check('a failed connection is reported', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, controlConnection: { stage: 'failed' } }), 'This laptop could not connect')
check('an idle laptop off the pods leaves the line empty', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, controlConnection: { stage: 'waiting' } }), '')
check('a connecting laptop says so', Model.localConnectionLabel({ ...localLinkStatus,
  connected: false, controlConnection: { stage: 'connecting' } }), 'This laptop is connecting')
{
  // 2026-10-08: the panel passed the service, whose lidState, leftPod and rightPod this label never reads.
  const inCasePod = { ...Model.defaultPod(), inCase: true, level: 80 }
  const closed = Model.statusLog(null, { t: 1000, ev: 'status', status: { ...Model.defaultStatus(), ok: true,
    caseLidState: Model.LID_CLOSED, left: inCasePod, right: inCasePod, controlConnection: { stage: 'waiting' } } })
  check('the status log view reads a closed case as in case', Model.localConnectionLabel(closed.view), 'AirPods in case')
}
{
  // The connection ring reads these from the view; the service's own copy merged a legacy file nothing writes.
  const native = (handoff, t) => ({ t, ev: 'status', status: { ...Model.defaultStatus(), ok: true, nativeHandoff: { updated_ms: t, ...handoff } } })
  let ring = Model.statusLog(null, native({ connecting: true, attempting: true, phase: 'control' }, 1000))
  check('the view carries a native connection in progress', [ring.view.connecting, ring.view.connectionAttempting,
    ring.view.handoffPhase, ring.view.connectionCompleted], [true, true, 'control', false])
  ring = Model.statusLog(ring, native({ connecting: false, completed: true, phase: 'idle' }, 2000))
  check('the view carries a completed native connection', [ring.view.connecting, ring.view.connectionCompleted], [false, true])
  const asking = Model.statusLog(null, { t: 1000, ev: 'status', status: { ...Model.defaultStatus(), ok: true,
    connected: false, fleet: { routing: { phase: 'waiting_admission', busy: true, peer: 'D0:C6:37:2F:D0:9F' } } } })
  check('asking the other laptop for the seat spins the ring', asking.view.connecting, true)
}
check('local Bluetooth overrides the fleet snapshot', Model.fleetSeatView({ ...fleetObserver, bluezConnected: true }), null)

const sharedHistory = Model.connectionPresentation({ ...Model.defaultStatus(), ...fleetObserver, peerListUpdated: 500 }, null, 1000)
const joiningFromFleet = { ...Model.defaultStatus(), ...fleetObserver, fleet: {}, connected: true,
  bluezConnected: true, controlConnection: {stage: 'notifications'}, peerListUpdated: 0 }
const sharedDuringJoin = Model.connectionPresentation(joiningFromFleet, sharedHistory, 2000)
check('fleet phone stays visible while local roster is missing',
  sharedDuringJoin.snapshot?.seats.apple[0]?.label, 'Phone')
// 2026-10-04: the joining laptop jumped into the seat before its own roster confirmed it.
check('joining laptop waits while the former helper leaves',
  sharedDuringJoin.snapshot?.seats.omarchy.map(p => p.label), [])
check('joining laptop is drawn waiting until its roster lands', sharedDuringJoin.snapshot?.localWaiting, true)
// 2026-10-04: the phone lost its glow while the joining laptop had no owner report of its own.
check('joining laptop keeps the fleet audio owner lit',
  [sharedDuringJoin.audio?.appleOwnsAudio, sharedDuringJoin.audio?.sourceAddress], [true, '02:00:00:00:00:02'])
// 2026-10-04: the glow blinked between the joining laptop's own roster and its first source report.
const joinedNoSource = Model.connectionPresentation({ ...joiningFromFleet, fleet: {},
  connectedPeers: ['02:00:00:00:00:02'], peerListUpdated: 2500, controlConnection: {stage: 'ready'} }, sharedDuringJoin, 2600)
check('fleet audio owner carries until the first local source report',
  [joinedNoSource.audio?.appleOwnsAudio, joinedNoSource.audio?.sourceAddress], [true, '02:00:00:00:00:02'])
const joinedWithSource = Model.connectionPresentation({ ...joiningFromFleet, fleet: {}, connectedPeers: ['02:00:00:00:00:02'],
  peerListUpdated: 2500, controlConnection: {stage: 'ready'}, audioOwner: REMOTE_PLAYING, remoteSourceAddress: '02:00:00:00:00:02' }, joinedNoSource, 2800)
check('the first local source report ends the carry', joinedWithSource.audio, null)
// 2026-10-04: the yielding laptop lost the glow on the helper's pending roster and the gap after it.
const holdingLive = { ...Model.defaultStatus(), ...fleetObserver, fleet: {}, connected: true, bluezConnected: true,
  connectedPeers: ['02:00:00:00:00:02'], peerListUpdated: 100, audioOwner: REMOTE_PLAYING, remoteSourceAddress: '02:00:00:00:00:02', controlConnection: {stage: 'ready'} }
const yieldRouting = { phase: 'waiting_connection', busy: true, peer: '02:00:00:00:00:01' }
const beforeYield = Model.connectionPresentation(holdingLive, null, 10000)
const yieldingHolder = Model.connectionPresentation({ ...holdingLive, connected: false, bluezConnected: false,
  connectedPeers: [], fleet: { routing: yieldRouting } }, beforeYield, 10700)
const pendingRoster = Model.connectionPresentation({ ...holdingLive, connected: false, bluezConnected: false,
  connectedPeers: [], fleet: { routing: yieldRouting, roster: { reporter: '02:00:00:00:00:01',
    peers: ['02:00:00:00:00:01', '02:00:00:00:00:02'], pending: true, audio_source: '' } } }, yieldingHolder, 15500)
check('a pending helper roster keeps the last audio owner',
  [pendingRoster.audio?.appleOwnsAudio, pendingRoster.audio?.sourceAddress], [true, '02:00:00:00:00:02'])
const rosterGap = Model.connectionPresentation({ ...holdingLive, connected: false, bluezConnected: false,
  connectedPeers: [], fleet: { routing: { phase: 'success', busy: false, peer: '02:00:00:00:00:01' } } }, pendingRoster, 17500)
check('the gap between helper rosters keeps the last audio owner', rosterGap.audio?.appleOwnsAudio, true)
const longGap = Model.connectionPresentation({ ...holdingLive, connected: false, bluezConnected: false,
  connectedPeers: [], fleet: { routing: { phase: 'success', busy: false, peer: '02:00:00:00:00:01' } } }, rosterGap, 26000)
check('the last audio owner expires 15 s after it was last confirmed', longGap.audio, null)
// 2026-10-07: the phone kept its seat through a laptop switch, but its playing glow went out until the switch landed.
const switchSeat = (status, presentation) => Model.seatPresentation({ ...status, daemonReachable: true,
  connectionPresentation: presentation }, false, false).audioPlaying
const yieldingLive = { ...holdingLive, connected: false, bluezConnected: false, connectedPeers: [] }
check('a playing phone stays lit while this laptop hands over', switchSeat({ ...yieldingLive,
  fleet: { routing: yieldRouting } }, yieldingHolder), true)
check('a playing phone stays lit on the helper\'s pending roster', switchSeat({ ...yieldingLive, fleet: { routing: yieldRouting,
  roster: { reporter: '02:00:00:00:00:01', peers: ['02:00:00:00:00:01', '02:00:00:00:00:02'], pending: true, audio_source: '' } } },
  pendingRoster), true)
check('a playing phone stays lit while this laptop joins', switchSeat(joiningFromFleet, sharedDuringJoin), true)
const idleHistory = Model.connectionPresentation({ ...Model.defaultStatus(), ...fleetObserver, peerListUpdated: 500,
  fleet: { roster: { ...fleetObserver.fleet.roster, audio_idle: true } } }, null, 1000)
// 2026-10-07 16:22: joining beside a phone that had played throughout logged "Audio moved to the Apple fleet".
const offPods = { ...Model.defaultStatus(), ...fleetObserver, nativeHandoff: { audio_output: 'speakers', audio_active: false } }
const joinedBesidePhone = { ...offPods, connected: true, connectedPeers: ['02:00:00:00:00:02'], audioOwner: REMOTE_PLAYING,
  remoteSourceAddress: '02:00:00:00:00:02', fleet: {} }
const watchedFromFleet = Model.audioRouteState(Model.audioRouteState(null, offPods), offPods)
check('joining beside the phone the fleet already reported playing is not a move',
  Model.audioRouteState(watchedFromFleet, joinedBesidePhone).message, '')
// 2026-10-07 16:51: leaving beside a phone that played throughout logged playing, ready, playing.
const besidePhone = Model.audioRouteState(Model.audioRouteState(null, offPods), joinedBesidePhone)
const leftPods = Model.audioRouteState(besidePhone, offPods)
check('leaving the pods beside the playing phone is not news', leftPods.message, besidePhone.message)
const idleFlap = Model.audioRouteState(leftPods, { ...offPods, fleet: { roster: { ...fleetObserver.fleet.roster, audio_idle: true } } })
check('a broadcast idle flap is not news', [idleFlap.message, Model.audioRouteState(idleFlap, offPods).message],
  [besidePhone.message, besidePhone.message])
const laptopTookOver = Model.audioRouteState(leftPods, { ...offPods,
  fleet: { roster: { ...fleetObserver.fleet.roster, audio_source: '02:00:00:00:00:01' } } })
check('a move the broadcast shows off the pods is logged', laptopTookOver.message, 'AirPods audio moved to laptop-a.')
const laptopHeldFleet = { ...offPods, fleet: { roster: { ...fleetObserver.fleet.roster, audio_source: '02:00:00:00:00:01' } } }
check('joining to find the phone took over from a laptop is still a move',
  Model.audioRouteState(Model.audioRouteState(null, laptopHeldFleet), joinedBesidePhone).message,
  'AirPods audio moved to Phone.')
// 2026-10-07 16:30: the joining laptop logged the switch twice, before "Connecting to AirPods." and "AirPods connected.".
const routingAt = (phase, busy) => ({ ok: true, localAddress: '02:00:00:00:00:03',
  fleet: { routing: { phase, busy, peer: '02:00:00:00:00:01', retain: '02:00:00:00:00:04' } } })
const resultAt = (before, after) => (Model.fleetOutcome(before, after) || { result: null }).result
check('a laptop that took the slot reports the switch once it is done',
  ['waiting_admission', 'connecting', 'waiting_completion'].map(p => resultAt(routingAt(p, true), routingAt('success', false))),
  ['joined', 'joined', 'joined'])
check('a finished switch is reported only on the change', resultAt(routingAt('success', false), routingAt('success', false)), null)
check('a failed request reports no switch', resultAt(routingAt('connecting', true), routingAt('failed', false)), 'join_failed')
// 2026-10-07 20:41: the XPS handed the seat to lg-gram, missed lg-gram's pending roster, and logged no switch.
check('a laptop that admitted another reports the switch once it is done',
  ['admitting', 'waiting_connection'].map(p => resultAt(routingAt(p, true), routingAt('success', false))), ['admitted', 'admitted'])
// A confirmed pair is visible even when the UI misses the fleet transaction's completion.
for (const joining of [false, true]) {
  const local = '02:00:00:00:00:01', peer = '02:00:00:00:00:02'
  const base = { ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
    localAddress: local, localName: joining ? 'laptop-b' : 'laptop-a', peerListUpdated: 1000,
    controlConnection: { stage: 'ready' }, devices: { [peer]: { kind: 'omarchy', label: joining ? 'laptop-a' : 'laptop-b' } },
    connectedPeers: [local] }
  const before = joining ? { ...base, connected: false, controlConnection: { stage: 'connecting' }, connectedPeers: [] } : base
  let state = Model.statusLog(null, { t: 1000, ev: 'status', status: before })
  const pair = { ...base, connectedPeers: [local, peer], peerListUpdated: 2000 }
  for (const unconfirmed of [
    { ...pair, controlConnection: { stage: 'handshake' } },
    { ...pair, peerListUpdated: 0 },
    { ...pair, devices: { [peer]: { kind: 'apple', label: 'Phone' } } }
  ]) {
    const held = Model.statusLog(state, { t: 1500, ev: 'status', status: unconfirmed })
    check('an incomplete or Apple pair cannot announce a fleet join',
      held.rows.some(r => r.message.includes(' connected alongside ')), false)
  }
  const opened = Model.statusLog(null, { t: 1500, ev: 'status', status: pair })
  check('opening the panel on an existing pair does not invent a joining order',
    opened.rows.some(r => r.message.includes(' connected alongside ')), false)
  state = Model.statusLog(state, { t: 2000, ev: 'status', status: pair })
  check('only the joining laptop adds its connected peer to the success line',
    state.rows.some(r => r.message === 'Connected to AirPods alongside laptop-a.'), joining)
  check('the pair is a typed join event on the ' + (joining ? 'joiner' : 'helper'),
    state.rows.filter(r => r.event && /Alongside$/.test(r.event.kind)).map(r => ({ ...r.event })),
    [joining ? { kind: 'joinedAlongside', peer: 'laptop-a' } : { kind: 'pairedAlongside', device: 'laptop-b', peer: 'laptop-a' }])

  const pairLines = () => state.rows.filter(r => r.message === 'laptop-b connected alongside laptop-a.')
  check('a confirmed pair logs the joining laptop on the ' + (joining ? 'joiner' : 'helper'), pairLines().length, joining ? 0 : 1)
  state = Model.statusLog(state, { t: 3000, ev: 'status', status: pair })
  check('repeated pair reports do not repeat the join', pairLines().length, joining ? 0 : 1)
  const completed = { ...pair, fleet: { routing: { phase: joining ? 'waiting_completion' : 'waiting_connection',
    busy: true, peer, retain: joining ? peer : local } } }
  state = Model.statusLog(state, { t: 4000, ev: 'status', status: completed })
  state = Model.statusLog(state, { t: 5000, ev: 'status', status: { ...completed,
    fleet: { routing: { ...completed.fleet.routing, phase: 'success', busy: false } } } })
  check('later fleet completion does not repeat the confirmed pair', pairLines().length, joining ? 0 : 1)
  if (joining) check('the joining success stays a single row through repeated confirmations',
    state.rows.filter(r => r.message === 'Connected to AirPods alongside laptop-a.').length, 1)
}
// A successful join can retain the playing helper instead of replacing its connection.
for (const joining of [true, false]) {
  const local = joining ? '02:00:00:00:00:03' : '02:00:00:00:00:01'
  const peer = joining ? '02:00:00:00:00:01' : '02:00:00:00:00:03'
  const event = (phase, t) => ({ t, ev: 'status', status: { ...Model.defaultStatus(), ...fleetObserver,
    ok: true, connected: true, bluezConnected: true, localAddress: local,
    localName: joining ? 'laptop-c' : 'laptop-a', audioOwner: joining ? REMOTE_PLAYING : LOCAL_PLAYING,
    remoteSourceAddress: '02:00:00:00:00:01', controlConnection: { stage: 'ready' },
    peerListUpdated: t, connectedPeers: phase === 'success' ? [local, peer] : [local],
    fleet: { routing: { phase, busy: phase !== 'success', peer, retain: '02:00:00:00:00:01' } } } })
  let join = Model.statusLog(null, event(joining ? 'waiting_completion' : 'waiting_connection', 1000))
  join = Model.statusLog(join, event('success', 2000))
  check('a retained helper means a join on the ' + (joining ? 'requester' : 'helper'),
    join.rows.some(r => r.message === 'laptop-c connected alongside laptop-a.'), !joining)
  if (joining) check('the late fleet confirmation updates the existing success row',
    join.rows.filter(r => !r.message.startsWith('AirPods audio ')).map(r => r.message), ['Connected to AirPods alongside laptop-a.'])
  check('a retained helper never reports its connection switched away',
    join.rows.some(r => r.message === 'Omarchy Fleet connection switched to laptop-c.'), false)
}
{
  const helper = (routing, t) => ({ t, ev: 'status', status: { ...Model.defaultStatus(), ok: true, ...fleetObserver, fleet: { routing },
    connected: false, lastAdvertisementMs: t, controlConnection: { stage: 'idle' } } })
  let state = Model.statusLog(null, helper({ phase: 'admitting', busy: true, peer: '02:00:00:00:00:01' }, 1000))
  state = Model.statusLog(state, helper({ phase: 'waiting_connection', busy: true, peer: '02:00:00:00:00:01' }, 2000))
  state = Model.statusLog(state, helper({ phase: 'success', busy: false, peer: '02:00:00:00:00:01' }, 3000))
  // 2026-10-07 20:49:07: the XPS's switch failed after its setup timed out, and the log only froze on "Retrying 1 of 2".
  const asking = (routing, t) => ({ t, ev: 'status', status: { ...Model.defaultStatus(), ok: true, ...fleetObserver,
    localName: 'omarchy-xps', fleet: { routing }, connected: false, lastAdvertisementMs: t, controlConnection: { stage: 'idle' } } })
  let failing = Model.statusLog(null, asking({ phase: 'waiting_completion', busy: true, peer: '02:00:00:00:00:01' }, 1000))
  failing = Model.statusLog(failing, asking({ phase: 'failed', busy: false, peer: '02:00:00:00:00:01', error: 'connection_failed' }, 2000))
  check('a failed switch says so in red', failing.rows.filter(r => r.kind === 'error').map(r => r.message),
    ["omarchy-xps couldn't connect to the AirPods."])
  // The route window can close mid-attempt; the attempt finishes its retries in the freed slot before anything is red.
  const retrying = (routing, stage, t, extra) => ({ ...asking(routing, t), status: { ...asking(routing, t).status,
    controlConnection: { stage, recovering: stage !== 'connecting', retry: 1, retry_limit: 2, ...(extra || {}) } } })
  const lapsed = { phase: 'failed', busy: false, peer: '02:00:00:00:00:01', error: 'connection_failed' }
  let late = Model.statusLog(null, retrying({ phase: 'connecting', busy: true, peer: '02:00:00:00:00:01' }, 'retrying', 1000))
  late = Model.statusLog(late, retrying(lapsed, 'connecting', 2000))
  check('a lapsed switch still retrying is not red yet', late.rows.filter(r => r.kind === 'error').length, 0)
  const failedLate = Model.statusLog(late, retrying(lapsed, 'failed', 3000, { reason: 'control setup timeout' }))
  check('its last retry failing is one red line', failedLate.rows.filter(r => r.kind === 'error').map(r => r.message),
    ["omarchy-xps couldn't connect to the AirPods."])
  const savedLate = Model.statusLog(late, { ...retrying(lapsed, 'ready', 3000), status: { ...retrying(lapsed, 'ready', 3000).status, connected: true } })
  check('its last retry connecting is not red', savedLate.rows.filter(r => r.kind === 'error').length, 0)
  let fallback = Model.statusLog(null, asking({ phase: 'waiting_admission', busy: true, peer: '02:00:00:00:00:01' }, 1000))
  fallback = Model.statusLog(fallback, asking({ phase: 'failed', busy: false, peer: '02:00:00:00:00:01', error: 'control_unavailable' }, 2000))
  check('a switch that falls back to a direct connect is not a failure', fallback.rows.filter(r => r.kind === 'error').length, 0)
  let busyPeer = null
  for (let i = 0; i < 4; ++i) {
    busyPeer = Model.statusLog(busyPeer, asking({ phase: 'waiting_admission', busy: true, peer: '02:00:00:00:00:01' }, 1000 + i * 2000))
    busyPeer = Model.statusLog(busyPeer, asking({ phase: 'failed', busy: false, peer: '02:00:00:00:00:01', error: 'peer_busy' }, 1500 + i * 2000))
  }
  check('a busy helper is not a failed Bluetooth connection', busyPeer.rows.filter(r => r.kind === 'error').length, 0)
  check('repeated busy replies produce one waiting message',
    busyPeer.rows.filter(r => r.message === 'Waiting for laptop-a...').length, 1)
  check('the giving laptop waits for a seat report before logging completion',
    state.rows.map(r => r.message).includes('Omarchy Fleet connection switched to laptop-a.'), false)
  check('a completion without a roster has no premature switch event', state.rows.map(r => r.event).filter(e => e && e.kind === 'fleetSwitched'), [])
}
// A daemon with fleet_outcome names how each transaction ended, so the log needs no busy phase to see it end.
{
  const local = '02:00:00:00:00:03', helper = '02:00:00:00:00:01', phone = '02:00:00:00:00:04'
  const at = (t, result, extra) => ({ t, ev: 'status', status: { ...Model.defaultStatus(), ...fleetObserver, ok: true,
    localAddress: local, localName: 'laptop-c', connected: false, lastAdvertisementMs: t, controlConnection: { stage: 'idle' },
    fleet: { routing: { phase: 'idle', busy: false } },
    fleetOutcome: result ? { atMs: t, result, peer: helper, retain: result.endsWith('_alongside') ? (result.startsWith('joined') ? helper : local) : phone } : null,
    ...(extra || {}) } })
  const run = (...results) => {
    let state = Model.statusLog(null, at(1000, null))
    results.forEach((result, i) => { state = Model.statusLog(state, at(2000 + i * 1000, result)) })
    return state
  }
  const kinds = (state) => state.rows.map(r => r.event && r.event.kind).filter(Boolean)
  check('a daemon join is logged as this laptop taking the seat', run('joined').rows.some(r => r.event && r.event.kind === 'fleetSwitched'
    && r.event.device === 'laptop-c'), true)
  check('a daemon join beside the helper is a typed success', run('joined_alongside').rows
    .filter(r => r.kind === 'success').map(r => r.event), [{ kind: 'joinedAlongside', peer: 'laptop-a' }])
  const arrived = Model.statusLog(run('admitted'), at(3000, 'admitted', {
    fleet: { roster: { reporter: helper, peers: [helper, phone], pending: false } }
  }))
  check('a daemon admission names the laptop when its seat report arrives', arrived.rows.map(r => r.event)
    .filter(e => e && e.kind === 'fleetSwitched'), [{ kind: 'fleetSwitched', device: 'laptop-a' }])
  check('an alongside admission waits for its pair before announcing it', run('admitted_alongside').rows.map(r => r.event)
    .filter(e => e && e.kind === 'pairedAlongside'), [])
  check('repeated daemon busy replies wait once', kinds(run('peer_busy', 'peer_busy', 'peer_busy')).filter(k => k === 'peerBusy').length, 1)
  check('a daemon join failure is one red typed line', run('join_failed').rows.filter(r => r.kind === 'error').map(r => r.event),
    [{ kind: 'switchFailed', device: 'laptop-c' }])
  check('a daemon fallback to a direct connect is quiet', run('peer_unavailable').rows.filter(r => r.kind === 'error').length, 0)
  check('a daemon outcome the panel already logged is not logged again',
    kinds(Model.statusLog(arrived, at(3000, 'admitted', { lastAdvertisementMs: 3500 }))).filter(k => k === 'fleetSwitched').length, 1)
  let startup = Model.statusLog(null, at(5000, 'join_failed'))
  check('an outcome from before the panel started is not logged', startup.rows.filter(r => r.kind === 'error').length, 0)
  check('the step a daemon failure arrives on reports it for the activity hold',
    [Model.fleetRouteFailed(run('join_failed').routeEnded), Model.fleetRouteFailed(Model.statusLog(run('join_failed'), { t: 9000, ev: 'tick' }).routeEnded)],
    [true, false])
}
// The log decides on event kinds; these pin the kind each key transition produces, apart from its wording.
{
  const local = '02:00:00:00:00:01', phone = 'AA:AA:AA:AA:AA:01'
  const base = { ...Model.defaultStatus(), ok: true, localAddress: local, localName: 'laptop-a', lastAdvertisementMs: 1000,
    devices: { [local]: { label: 'laptop-a', kind: 'omarchy' }, [phone]: { label: 'iPhone', kind: 'apple' } } }
  const linked = { ...base, connected: true, bluezConnected: true, connectedPeers: [local], controlConnection: { stage: 'ready' } }
  const phonePlaying = { ...linked, audioOwner: REMOTE_PLAYING, remoteSourceAddress: phone }
  const docked = { ...base, caseConfirmedDocked: true, caseLidState: Model.LID_CLOSED, lidState: Model.LID_CLOSED,
    lastAdvertisementMs: 10000, controlConnection: { stage: 'idle' } }
  const kinds = (s) => s.rows.map(r => r.event && r.event.kind)
  let s = Model.statusLog(null, { t: 1000, ev: 'status', status: { ...base, controlConnection: { stage: 'connecting' } } })
  s = Model.statusLog(s, { t: 2000, ev: 'status', status: { ...linked, audioOwner: LOCAL } })
  check('a ready control link completes the attempt as a connected event', kinds(s)[0], 'connected')
  check('this laptop holding the audio is an audio-on caption', s.caption, { kind: 'audioOn', device: 'laptop-a' })
  let handoff = Model.statusLog(s, { t: 3000, ev: 'status', status: { ...phonePlaying, nativeHandoff: { phase: 'idle' } } })
  handoff = Model.statusLog(handoff, { t: 9000, ev: 'tick' })
  s = Model.statusLog(s, { t: 3000, ev: 'status', status: phonePlaying })
  s = Model.statusLog(s, { t: 9000, ev: 'tick' })
  check('the phone holding the audio is an Apple fleet event', s.caption, { kind: 'appleHasAudio', device: 'laptop-a', seated: true })
  check('each caption is logged with its kind', kinds(s), ['connected', 'audioOn', 'appleHasAudio'])
  check('a native handoff keeps the Apple fleet caption out of the log',
    [handoff.caption.kind, kinds(handoff)], ['appleHasAudio', ['connected', 'audioOn']])
  s = Model.statusLog(s, { t: 10000, ev: 'status', status: docked })
  check('a closed case has its own caption and clears the old connection log',
    [s.caption, kinds(s)], [{ kind: 'caseClosed' }, []])
}
check('a paused phone stays unlit while this laptop joins', switchSeat(joiningFromFleet,
  Model.connectionPresentation(joiningFromFleet, idleHistory, 2000)), false)
const askingFromFleet = { ...Model.defaultStatus(), ...fleetObserver, connected: false, bluezConnected: false,
  controlConnection: {stage: 'idle'}, fleet: { routing: { phase: 'waiting_admission', busy: true, peer: '02:00:00:00:00:01' } } }
// 2026-10-07: an idle phone gives up its slot, so the helper stays while the phone is known to be idle.
// 2026-10-12: with no source named, asking for the seat empties it at once, as when the other laptop takes it.
check('a helper beside an unknown Apple fleet leaves when asked', Model.connectionPresentation(askingFromFleet,
  sharedHistory, 1500).snapshot?.seats.omarchy.map(p => p.label), [])
const idlePhoneRequest = { ...requesting, fleet: { roster: { ...fleetObserver.fleet.roster, audio_idle: true }, routing: requestRouting } }
check('a helper beside an idle phone stays while the request is pending',
  Model.fleetSeatView(idlePhoneRequest)?.seats.omarchy.map(p => p.label), ['laptop-a'])
// 2026-10-04 16:47:16: a late helper roster put it back in the seat until this laptop's link came up.
const releasedWithStaleRoster = { ...askingFromFleet, fleet: { roster: fleetObserver.fleet.roster,
  routing: { phase: 'connecting', busy: true, peer: '02:00:00:00:00:01' } } }
check('a released helper roster no longer drives the seats', Model.fleetSeatView(releasedWithStaleRoster), null)
check('the joining view holds against a late helper roster', Model.connectionPresentation(releasedWithStaleRoster,
  sharedHistory, 1500).snapshot?.seats.omarchy.map(p => p.label), [])
check('the helper leaves the seat once it has let go', Model.connectionPresentation({ ...askingFromFleet,
  fleet: { routing: { phase: 'connecting', busy: true, peer: '02:00:00:00:00:01' } } }, sharedHistory, 1500)
  .snapshot?.seats.omarchy.map(p => p.label), [])
// 2026-10-05 18:18:42: the XPS kept playing and admitted lg-gram into the iPhone's slot, but the joining view
// emptied the Omarchy seat as if the helper had left. The admission names the device the helper keeps.
const retainingHelper = (retain) => Model.connectionPresentation({ ...askingFromFleet,
  fleet: { routing: { phase: 'connecting', busy: true, peer: '02:00:00:00:00:01', retain } } }, sharedHistory, 1500).snapshot?.seats
check('a helper that keeps its own link stays seated and the other device leaves',
  [retainingHelper('02:00:00:00:00:01')?.omarchy.map(p => p.label), retainingHelper('02:00:00:00:00:01')?.apple.map(p => p.label)],
  [['laptop-a'], []])
check('a helper that keeps the phone leaves and the phone stays',
  [retainingHelper('02:00:00:00:00:02')?.omarchy.map(p => p.label), retainingHelper('02:00:00:00:00:02')?.apple.map(p => p.label)],
  [[], ['Phone']])
check('remembered fleet state is explicitly unconfirmed', sharedDuringJoin.snapshot?.unconfirmed, true)
check('fresh local roster ends provisional fleet view', Model.connectionPresentation({ ...joiningFromFleet,
  peerListUpdated: 2100, connectedPeers: [], controlConnection: {stage:'ready'} }, sharedDuringJoin, 2200).snapshot, null)
check('fleet handoff memory expires', Model.connectionPresentation(joiningFromFleet, sharedDuringJoin, 33000).snapshot, null)
check('case closure clears fleet handoff memory', Model.connectionPresentation({ ...joiningFromFleet,
  caseConfirmedDocked: true }, sharedDuringJoin, 2200).snapshot, null)


const joinedBeforeFleet = { ...joiningFromFleet, fleet: fleetObserver.fleet }
const lateFleet = Model.connectionPresentation(joinedBeforeFleet, null, 2000)
check('fleet roster arriving after Bluetooth still seats the phone', lateFleet.snapshot?.seats.apple[0]?.onAcl, true)
check('late fleet roster keeps the joining laptop waiting', [lateFleet.snapshot?.seats.omarchy.length, lateFleet.snapshot?.localWaiting], [0, true])
const audioBeforeRoster = { ...joiningFromFleet, audioOwner: REMOTE_PLAYING, remoteSourceAddress: '02:00:00:00:00:02' }
const earlyAudio = Model.connectionPresentation(audioBeforeRoster, sharedDuringJoin, 2200)
check('audio report cannot erase the fleet phone seat before roster arrives', earlyAudio.snapshot?.seats.apple[0]?.onAcl, true)
const freshEmptyRoster = Model.connectionPresentation({ ...joinedBeforeFleet,
  peerListUpdated: 2200, connectedPeers: [] }, lateFleet, 2300)
check('fresh empty direct roster overrides a still-advertised fleet pair', freshEmptyRoster.snapshot, null)
check('early audio cannot extend the provisional roster deadline', Model.connectionPresentation(
  audioBeforeRoster, earlyAudio, 33000).snapshot?.unconfirmed || false, false)

// The direct roster can arrive before XPS finishes notifications. Switching
// from Gram's report to that roster must not send the retained phone to the tray.
{
  const xps = '02:00:00:00:00:03', gram = '02:00:00:00:00:01', phone = '02:00:00:00:00:02'
  const base = { ...Model.defaultStatus(), ...fleetObserver, ok: true }
  const routing = { phase: 'connecting', busy: true, peer: gram, retain: phone }
  const joining = { ...base, fleet: { routing }, controlConnection: { stage: 'connecting' } }
  const linked = { ...joining, connected: true, bluezConnected: true,
    controlConnection: { stage: 'notifications' } }
  const roster = { ...linked, connectedPeers: [phone, xps], peerListUpdated: 4000 }
  let state = null
  for (const [t, status] of [[1000, base], [2000, joining], [3000, linked], [4000, roster],
    [5000, { ...roster, controlConnection: { stage: 'ready' } }]]) {
    state = Model.statusLog(state, { t, ev: 'status', status })
    check('retained phone stays seated throughout XPS arrival at ' + t,
      state.seat.seats.apple.map(d => d.address), [phone])
    check('retained phone never returns to the tray at ' + t,
      state.seat.availableApple.some(d => d.address === phone), false)
    check('XPS waits for its ready connection at ' + t,
      state.seat.seats.omarchy.some(d => d.address === xps), t === 5000)
  }
  for (const peers of [[], [gram, xps]]) {
    const changed = Model.statusLog(state, { t: 6000, ev: 'status', status: {
      ...roster, connectedPeers: peers, peerListUpdated: 6000
    } })
    check('an explicit new roster can remove the phone during setup: ' + peers,
      changed.seat.seats.apple, [])
  }
}

const pendingFleet = Model.fleetSeatView({ ...fleetObserver,
  fleet: { roster: { ...fleetObserver.fleet.roster, pending: true } } })
check('pending fleet roster is distinguished from confirmed seats', pendingFleet.pending, true)
check('pending fleet roster seats the retained Apple device', pendingFleet.seats.apple[0]?.onAcl, true)
check('pending fleet roster does not claim current audio ownership', pendingFleet.appleOwnsAudio, false)
// Activity reconnects a displaced laptop; idle empty seats need no instruction label.
check('the playback wait is parsed', Model.parseStatus(JSON.stringify({ schema_version: 1, waiting_for_playback: true })).waitingForPlayback, true)
const waitingSeat = Model.seatPresentation({ ...Model.defaultStatus(), daemonReachable: true, waitingForPlayback: true,
  connectionPresentation: { refreshing: false, snapshot: null } }, false, false)
check('a laptop waiting for activity leaves the empty seat unlabelled', waitingSeat.omarchyTransitionLabel, '')
const connectedWaiting = Model.seatPresentation({ ...Model.defaultStatus(), daemonReachable: true, waitingForPlayback: true,
  connected: true, connectionPresentation: { refreshing: false, snapshot: null } }, false, false)
check('a connected laptop no longer waits', connectedWaiting.omarchyTransitionLabel, '')

const combinedFleet = Model.seatPresentation({ ...Model.defaultStatus(), ...fleetObserver,
  daemonReachable: true, connectionPresentation: { refreshing: false, snapshot: null } }, false, false)
const blankSeat = Model.seatPresentation({ ...Model.defaultStatus(), daemonReachable: true,
  connectionPresentation: { refreshing: false, snapshot: null } }, false, false)
check('empty seat presentation has explicit ownership and source values',
  [blankSeat.omarchyOwnsAudio, blankSeat.audioDeviceAddress, blankSeat.refreshingConnection],
  [false, '', false])
check('one fleet presentation chooses the seats, source and caption together',
  [combinedFleet.seats.apple[0]?.label, combinedFleet.appleOwnsAudio,
    combinedFleet.audioDeviceAddress, combinedFleet.audioSubtitle],
  ['Phone', true, '02:00:00:00:00:02', 'Apple fleet · Phone · Playing'])
const combinedPending = Model.seatPresentation({ ...Model.defaultStatus(), ...fleetObserver,
  daemonReachable: true, fleet: { roster: { ...fleetObserver.fleet.roster, pending: true } },
  connectionPresentation: { refreshing: false, snapshot: null } }, false, false)
check('pending fleet seats do not invent an owner or caption',
  [combinedPending.seats.apple[0]?.label, combinedPending.appleOwnsAudio,
    combinedPending.omarchyOwnsAudio, combinedPending.audioSubtitle],
  ['Phone', false, false, ''])
const combinedJoin = Model.seatPresentation({ ...joiningFromFleet, daemonReachable: true,
  connectionPresentation: sharedDuringJoin }, false, false)
check('joining keeps the same remembered phone in its seat and caption',
  [combinedJoin.seats.apple[0]?.label, combinedJoin.appleOwnsAudio,
    combinedJoin.audioDeviceAddress, combinedJoin.audioSubtitle],
  ['Phone', true, '02:00:00:00:00:02', 'Apple fleet · Phone · Playing'])
check('joining keeps the switch on the seat while retaining the audio caption',
  [combinedJoin.switching, combinedJoin.audioSubtitle], [true, 'Apple fleet · Phone · Playing'])
check('fleet handoff between laptops says switching', combinedJoin.omarchyTransitionLabel, 'Switching…')
const directAppleJoin = Model.seatPresentation({ ...Model.defaultStatus(), daemonReachable: true,
  localAddress: '02:00:00:00:00:03', localName: 'omarchy-xps',
  devices: { '02:00:00:00:00:01': { label: 'Mac', kind: 'apple' },
    '02:00:00:00:00:02': { label: 'iPhone', kind: 'apple' } },
  controlConnection: { stage: 'connecting' },
  connectionPresentation: { refreshing: false, snapshot: null } }, false, false)
check('desktop activity joining an empty Omarchy seat says connecting',
  [directAppleJoin.switching, directAppleJoin.omarchyTransitionLabel], [true, 'Connecting…'])
const reconnectingSelf = Model.seatPresentation({ ...Model.defaultStatus(), daemonReachable: true,
  localAddress: '02:00:00:00:00:03', localName: 'omarchy-xps',
  controlConnection: { stage: 'connecting' },
  connectionPresentation: { refreshing: true,
    snapshot: { unconfirmed: true, seats: { apple: [], omarchy: [] } },
    lastKnown: { seats: { omarchy: [{ address: '02:00:00:00:00:03', label: 'omarchy-xps' }] } } } },
  false, false)
check('rejoining this laptop does not count as replacing another Omarchy seat',
  reconnectingSelf.omarchyTransitionLabel, 'Connecting…')
const combinedDirectRoster = Model.seatPresentation({ ...joinedBeforeFleet,
  daemonReachable: true, peerListUpdated: 2200, connectedPeers: [],
  connectionPresentation: freshEmptyRoster }, false, false)
check('fresh empty direct roster during setup cannot retain the old audio owner',
  [combinedDirectRoster.seats.apple.length, combinedDirectRoster.appleOwnsAudio,
    combinedDirectRoster.audioDeviceAddress], [0, false, ''])

check('Bluetooth alone does not claim a confirmed local connection', Model.localConnectionLabel({ ...localLinkStatus,
  connected: true, bluezConnected: true, controlConnection: {stage:'notifications'} }), 'This laptop is waiting for the AirPods to confirm')

// 2026-10-04: lg-gram held the idle audio but the XPS drew no owner; the pods' roster names it.
const idleOwnerStatus = { ...Model.defaultStatus(), connected: true, localAddress: '02:00:00:00:00:03',
  devices: { '02:00:00:00:00:01': { label: 'laptop-a', kind: 'omarchy' }, '02:00:00:00:00:02': { label: 'Phone', kind: 'apple' } },
  audioOwner: UNKNOWN, rosterAudioOwner: '02:00:00:00:00:01' }
check('an idle roster owner on another laptop is the audio owner', Model.omarchyRosterOwner(idleOwnerStatus), '02:00:00:00:00:01')
check('a playing source outranks the roster owner',
  Model.omarchyRosterOwner({ ...idleOwnerStatus, audioOwner: REMOTE_PLAYING, remoteSourceAddress: '02:00:00:00:00:02' }), '')
// Both laptops must agree, so this laptop is drawn as the owner too, even after it stepped back to its speakers.
check('this laptop owning idle audio is the owner', Model.omarchyRosterOwner({ ...idleOwnerStatus, rosterAudioOwner: '02:00:00:00:00:03' }), '02:00:00:00:00:03')
check('an Apple roster owner is not an Omarchy owner', Model.omarchyRosterOwner({ ...idleOwnerStatus, rosterAudioOwner: '02:00:00:00:00:02' }), '')
check('no roster owner without a local link', Model.omarchyRosterOwner({ ...idleOwnerStatus, connected: false }), '')
// 2026-10-04: a paused iPhone still holding the audio glows like an idle laptop owner.
check('a paused Apple roster owner is the Apple audio owner', Model.appleRosterOwner({ ...idleOwnerStatus, rosterAudioOwner: '02:00:00:00:00:02' }), '02:00:00:00:00:02')
check('a laptop roster owner is not an Apple owner', Model.appleRosterOwner(idleOwnerStatus), '')
check('a playing source outranks an Apple roster owner',
  Model.appleRosterOwner({ ...idleOwnerStatus, rosterAudioOwner: '02:00:00:00:00:02', audioOwner: LOCAL_PLAYING }), '')
check('no Apple roster owner without a local link', Model.appleRosterOwner({ ...idleOwnerStatus, rosterAudioOwner: '02:00:00:00:00:02', connected: false }), '')
// 2026-10-05: after the XPS paused and released, the pods handed lg-gram the audio, but the XPS's own stale
// "streaming here" report hid it. The daemon resolves that release as remote and not playing.
check('a release does not hide the roster owner', Model.omarchyRosterOwner({ ...idleOwnerStatus, audioOwner: REMOTE }), '02:00:00:00:00:01')
check('a playing local source outranks the roster owner',
  Model.omarchyRosterOwner({ ...idleOwnerStatus, audioOwner: LOCAL_PLAYING }), '')
check('a release does not hide a paused Apple owner',
  Model.appleRosterOwner({ ...idleOwnerStatus, rosterAudioOwner: '02:00:00:00:00:02', audioOwner: REMOTE }), '02:00:00:00:00:02')
// A release only says this laptop stopped owning; with another laptop flagged in the roster, Apple holds nothing.
const releasedStatus = { ...idleOwnerStatus, audioOwner: REMOTE }
check('a release to another laptop is not Apple holding the audio', Model.appleOwnsAudio(releasedStatus), false)
check('a release with no laptop owner leaves Apple holding the audio', Model.appleOwnsAudio({ ...releasedStatus, rosterAudioOwner: '' }), true)
check('a live remote source means Apple holds the audio',
  Model.appleOwnsAudio({ ...releasedStatus, audioOwner: REMOTE_PLAYING }), true)
check('a live local source means Apple does not', Model.appleOwnsAudio({ ...releasedStatus, audioOwner: LOCAL_PLAYING }), false)
const releasedPresentation = Model.connectionPresentation(releasedStatus, null, 1000)
check('a release to another laptop is remembered as that laptop holding the audio',
  [releasedPresentation.lastAudio.appleOwnsAudio, releasedPresentation.lastAudio.sourceAddress, releasedPresentation.lastKnown.linuxHeld],
  [false, '02:00:00:00:00:01', true])
check('the roster owner is parsed from status', Model.parseStatus(JSON.stringify({ schema_version: 1, roster_audio_owner: 'd0:c6:37:2f:d0:9f' })).rosterAudioOwner, 'D0:C6:37:2F:D0:9F')


// Seat layout.
check('a phone is named as one', [Model.isPhone('iPhone'), Model.isPhone("Dan's Phone"), Model.isPhone('MacBook'), Model.isPhone()], [true, true, false, false])
const PHONE = { address: '02:00:00:00:00:02', label: 'iPhone', onAcl: true }
const MAC = { address: '02:00:00:00:00:06', label: 'MacBook', onAcl: false }
const idleReconnect = { busy: false, connected: true, enabled: true }
const apple = Model.appleSeat([PHONE, MAC], [{ address: '02:00:00:00:00:07', label: 'iPad' }], '', idleReconnect)
check('the Apple seat seats devices on the link and trays the rest',
  [apple.devices.map(d => d.label), apple.waiting.map(d => d.label), apple.linkConnected, apple.showWaiting], [['iPhone'], ['MacBook', 'iPad'], true, true])
check('a lone seated device is the audio device when the source is elsewhere', apple.audioDeviceAddress, '02:00:00:00:00:02')
check('a seated source is the audio device', Model.appleSeat([PHONE, { ...MAC, onAcl: true }], [], '02:00:00:00:00:06', idleReconnect).audioDeviceAddress, '02:00:00:00:00:06')
const emptyApple = Model.appleSeat([MAC], [], '', idleReconnect)
check('an empty Apple seat with a waiting device offers Reconnect', [emptyApple.actionLabel, emptyApple.actionEnabled], ['Reconnect', true])
check('an empty Apple seat with nobody waiting still offers Reconnect',
  [Model.appleSeat([], [], '', idleReconnect).actionLabel, Model.appleSeat([], [], '', idleReconnect).actionEnabled],
  ['Reconnect', true])
check('an empty Apple seat offers nothing while this laptop is off the pods',
  Model.appleSeat([], [], '', { ...idleReconnect, connected: false }).actionLabel, '')
const busyApple = Model.appleSeat([MAC], [], '', { ...idleReconnect, busy: true })
check('a running Reconnect is labelled and disabled', [busyApple.actionLabel, busyApple.actionEnabled], ['Reconnecting…', false])
// 2026-10-07: with this laptop off the pods there is nothing to reconnect through, and the phone can join on its own.
check('Reconnect is not offered without this laptop connected', Model.appleSeat([MAC], [], '', { ...idleReconnect, connected: false }).actionLabel, '')
check('a running Reconnect stays labelled while this laptop drops off',
  Model.appleSeat([MAC], [], '', { ...idleReconnect, connected: false, busy: true }).actionLabel, 'Reconnecting…')
check('a seated Apple device leaves no action', apple.actionLabel, '')
// 2026-10-07: with a laptop and an Apple device connected, a Mac declined to take the pods unless the Apple device held
// the audio ("Tipi device, do not route"), so the Apple tray shows only then.
const SEATED_LAPTOP = { address: '02:00:00:00:00:09', label: 'omarchy-xps', onAcl: true }
check('the tray shows while the Apple device holds the audio', Model.appleTrayAvailable([PHONE], [SEATED_LAPTOP], true), true)
check('the tray hides while the laptop holds the audio', Model.appleTrayAvailable([PHONE], [SEATED_LAPTOP], false), false)
check('an empty Apple seat hides the connection affordance', Model.appleTrayAvailable([], [SEATED_LAPTOP], false), false)
check('a stale Apple owner cannot enable an empty seat tray', Model.appleTrayAvailable([], [SEATED_LAPTOP], true), false)
check('without confirmed Apple ownership the tray stays hidden', Model.appleTrayAvailable([PHONE], [], false), false)
check('an Apple owner can enable its tray without a laptop', Model.appleTrayAvailable([PHONE], [], true), true)
check('an unconfirmed Apple seat cannot enable the tray',
  Model.appleTrayAvailable([{ ...PHONE, onAcl: false }], [SEATED_LAPTOP], true), false)
const hiddenTray = Model.appleSeat([PHONE, MAC], [{ address: '02:00:00:00:00:07', label: 'iPad' }], '', idleReconnect, false)
check('a hidden tray seats the phone and waits no one', [hiddenTray.devices.map(d => d.label), hiddenTray.waiting, hiddenTray.showWaiting],
  [['iPhone'], [], false])
check('the tray opens for known Apple devices once this laptop holds the empty Apple seat',
  Model.appleTrayAvailable([], [SEATED_LAPTOP], false, [MAC], true), true)
check('the tray stays hidden with no Apple device to wait',
  Model.appleTrayAvailable([], [SEATED_LAPTOP], false, [], true), false)
check('the tray shows for a waiting Mac while the seated phone is not playing',
  Model.appleTrayAvailable([PHONE], [SEATED_LAPTOP], false, [MAC], true, false), true)
check('the tray stays hidden for a waiting Mac while the seated phone is playing',
  Model.appleTrayAvailable([PHONE], [SEATED_LAPTOP], false, [MAC], true, true), false)

const omarchy = Model.omarchySeat([{ address: 'B', label: 'xps' }, { address: 'A', label: 'gram' }], [], '', true, false, false)
check('without a known local laptop the pair falls back to label order', omarchy.devices.map(d => d.label), ['gram', 'xps'])
check('the local laptop takes the left slot of the pair',
  Model.omarchySeat([{ address: 'B', label: 'xps' }, { address: 'A', label: 'gram' }], [], '', true, false, false, 'B').devices.map(d => d.label),
  ['xps', 'gram'])
check('a held Omarchy seat shows its tray once ownership is known',
  [Model.omarchySeat([], [], '', true, true, false).showWaiting, Model.omarchySeat([], [], '', true, true, true).showWaiting], [true, false])
check('the Omarchy link follows this laptop or a seated peer',
  [Model.omarchySeat([], [], '', false, false, false).linkConnected, Model.omarchySeat([{ address: 'A', onAcl: true }], [], '', false, false, false).linkConnected], [false, true])

const XPS = { address: '02:00:00:00:00:01', label: 'xps', onAcl: true }
const GRAM = { address: '02:00:00:00:00:03', label: 'gram', onAcl: true }
let hold = Model.seatPairHold({ pair: [], emptied: false }, [GRAM, XPS], true)
check('a pair is held while Reconnect runs', hold.pair.map(d => d.label), ['gram', 'xps'])
hold = Model.seatPairHold(hold, [XPS], true)
const oneLeft = Model.seatedDevices([XPS], hold, true)
check('the laptop that left keeps its slot as an unseen stand-in',
  oneLeft.map(d => [d.label, !!d.standIn, d.onAcl]), [['gram', true, false], ['xps', false, true]])
check('without Reconnect the remaining laptop centers', Model.seatedDevices([XPS], Model.seatPairHold(hold, [XPS], false), false), [XPS])
hold = Model.seatPairHold(hold, [], true)
check('both gone leaves two stand-ins', [hold.emptied, Model.seatedDevices([], hold, true).every(d => d.standIn)], [true, true])
hold = Model.seatPairHold(hold, [GRAM], true)
check('one rejoining after the seat emptied is alone', Model.seatedDevices([GRAM], hold, true), [GRAM])
check('a stranger arriving during Reconnect is not held in a stand-in slot',
  Model.seatedDevices([PHONE], Model.seatPairHold({ pair: [], emptied: false }, [GRAM, XPS], true), true), [PHONE])

const W1 = { address: '01', label: 'a' }, W2 = { address: '02', label: 'b' }, W3 = { address: '03', label: 'c' }
check('the tray orders by departure', Model.trayDevices([W2, W1], { '01': 5, '02': 9 }).map(d => d.label), ['a', 'b'])
check('a full tray keeps the earliest and the latest departure', Model.trayDevices([W1, W2, W3], { '01': 1, '02': 3, '03': 2 }).map(d => d.label), ['a', 'b'])

check('the audio device is lit', Model.seatDeviceLit(PHONE, apple, true), true)
check('nothing is lit without the audio', Model.seatDeviceLit(PHONE, apple, false), false)
check('a device off the link is not lit', Model.seatDeviceLit({ ...PHONE, onAcl: false }, apple, true), false)
const pair = Model.omarchySeat([XPS, GRAM], [], '', true, false, false)
check('a pair with no named source lights neither', [Model.seatDeviceLit(XPS, pair, true), Model.seatDeviceLit(GRAM, pair, true)], [false, false])
const named = Model.omarchySeat([XPS, GRAM], [], '02:00:00:00:00:03', true, false, false)
check('a pair lights only the named source', [Model.seatDeviceLit(XPS, named, true), Model.seatDeviceLit(GRAM, named, true)], [false, true])

// Recovery must be news even when the last audible owner was already this laptop.
{
  const base = { ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
    localName: 'test-laptop', localAddress: '02:00:00:00:00:03', controlConnection: { stage: 'ready' },
    audioOwner: LOCAL_PLAYING, left: { ...Model.defaultPod(), inEar: true } }
  const event = (t, failure, playing) => ({ t, ev: 'status', status: { ...base,
    nativeHandoff: { failure, audio_active: playing, audio_output: playing ? 'airpods' : 'speakers',
      audio_state: playing ? 'playing' : 'profile_missing' } } })
  let log = Model.statusLog(null, event(1000, '', true))
  log = Model.statusLog(log, event(2000, 'profile_error:org.bluez.Error.InProgress', false))
  log = Model.statusLog(log, event(3000, '', false))
  check('clearing an audio error alone does not announce recovery', log.rows.filter(r => r.kind === 'success').length, 0)
  log = Model.statusLog(log, event(4000, '', true))
  check('confirmed playback supersedes the old audio error',
    [log.rows.at(-1).kind, log.rows.at(-1).message], ['success', 'test-laptop audio connection restored.'])
  log = Model.statusLog(log, event(5000, '', true))
  check('audio recovery is reported once', log.rows.filter(r => r.kind === 'success').length, 1)
  check('recovery preserves the failure history', log.rows.some(r => r.kind === 'error'), true)
}

// A remote disconnect can leave the daemon waiting for playback instead of attempting a connection.
{
  const status = { ...Model.defaultStatus(), ...fleetObserver, ok: true, waitingForPlayback: true,
    controlConnection: { stage: 'idle' }, nativeHandoff: { enabled: true }, hasAirPods: true }
  let log = Model.statusLog(null, { t: 1000, ev: 'status', status })
  check('a connection hold is explicit despite a nearby fleet roster', log.rows.at(-1)?.message,
    'laptop-c disconnected. Play audio or click to reconnect.')
  log = Model.statusLog(log, { t: 2000, ev: 'status', status: { ...status, controlConnection: { stage: 'connecting' } } })
  check('an actual attempt replaces the hold', log.rows.at(-1)?.live, true)
  log = Model.statusLog(log, { t: 3000, ev: 'status', status: { ...status, controlConnection: { stage: 'waiting' } } })
  check('a playback hold is not mistaken for waiting for the case', log.rows.at(-1)?.message,
    'laptop-c disconnected. Play audio or click to reconnect.')
}

{
  const status = { ...Model.defaultStatus(), ok: true, daemonReachable: true, connected: false,
    bluezConnected: false, waitingForPlayback: false, hasAirPods: true, localName: 'omarchy-xps',
    bleConnectionState: 5, lastAdvertisementMs: 1000, lidState: Model.LID_UNKNOWN,
    left: { ...Model.defaultPod(), inEar: true, level: 49 },
    right: { ...Model.defaultPod(), inEar: true, level: 52 },
    controlConnection: { stage: 'idle' } }
  const log = Model.statusLog(null, { t: 2000, ev: 'status', status })
  check('music on available pods is not this laptop disconnecting', log.rows.at(-1)?.message,
    'Connected elsewhere. The AirPods are available.')
  check('the header names the other connection', log.subtitle, 'Connected elsewhere')
}

// Waiting for the old link to release and opening the new link share one animated row.
{
  const peer = "02:00:00:00:00:01"
  const routing = { phase: "waiting_admission", busy: true, peer }
  const base = { ...Model.defaultStatus(), ...fleetObserver, ok: true, daemonReachable: true,
    lastAdvertisementMs: 1000, controlConnection: { stage: "idle" },
    fleet: { ...fleetObserver.fleet, routing } }
  let log = Model.statusLog(null, { t: 1000, ev: "status", status: base })
  const waiting = log.rows.find(r => r.live)
  check("a playing Apple peer requires the helper to release first", waiting?.message, "Waiting for laptop-a to disconnect...")
  log = Model.statusLog(log, { t: 1500, ev: "status", status: { ...base,
    fleet: { routing, roster: { ...base.fleet.roster, peers: [], audio_source: "" } } } })
  check("an empty helper roster does not finish the release wait", log.rows.find(r => r.live)?.event?.kind, "waitingDisconnect")
  log = Model.statusLog(log, { t: 1900, ev: "status", status: { ...base,
    fleet: { routing: { ...routing, phase: "connecting" } } } })
  check("admission before the socket attempt keeps the same live row",
    log.rows.find(r => r.live)?.id, waiting?.id)
  check("admission alone advances the row to connecting",
    log.rows.find(r => r.live)?.event?.kind, "connecting")
  log = Model.statusLog(log, { t: 2000, ev: "status", status: { ...base,
    controlConnection: { stage: "connecting" }, fleet: { routing: { ...routing, phase: "connecting" } } } })
  check("admission advances the existing row to connecting", log.rows.find(r => r.live)?.id, waiting?.id)
  check("the next step names the connecting laptop", log.rows.find(r => r.live)?.message, "Connecting laptop-c to AirPods...")
  for (const roster of [{ ...base.fleet.roster, audio_idle: true }, { ...base.fleet.roster, audio_source: peer }]) {
    const alongside = Model.statusLog(null, { t: 1000, ev: "status", status: { ...base, fleet: { routing, roster } } })
    check("a retained helper is not described as disconnecting", alongside.rows.some(r => r.event?.kind === "waitingDisconnect"), false)
  }
}

// Intentional Apple reconnect crosses interrupted/connecting snapshots once.
{
  let log = null
  for (const [index, stage] of ["connecting", "interrupted", "connecting", "retrying"].entries()) {
    log = Model.statusLog(log, { t: 1000 + index * 100, ev: "status", status: {
      ...Model.defaultStatus(), ok: true, daemonReachable: true, lidState: "open",
      controlConnection: { stage, peer_reconnect: true } } })
  }
  check("the Apple reconnect progress is not duplicated by its deliberate disconnect",
    log.rows.filter(r => r.message === "Trying to reconnect the Apple device...").length, 1)
}

// The status log's recording rules, carried over from tst_StatusHistory when the log moved into Model.statusLog.
{
  const newLog = () => { const s = Model.statusLogState(); s.now = 0; s.input = { controlConnection: null, refreshing: false,
    inCaseOnly: false, suppress: false, caption: null, audio: "", phase: "", error: "" }; return s }
  const record = (s, over) => { Object.assign(s.input, over || {}); Model.statusLogRecord(s, s.input); return s }
  const report = (s, stage, caption, recovering) => record(s, { controlConnection: { stage, recovering: recovering === true,
    retry: 1, retry_limit: 2 }, caption, refreshing: stage === "ready" })
  const notHere = { kind: "notConnected", device: "Linux laptop" }
  const inCase = { kind: "inCase" }
  const appleKeepsSeat = { kind: "appleHasAudio", device: "Linux laptop", seated: true }
  const lines = (s) => s.rows.map((r) => r.message)

  const retryLog = newLog()
  retryLog.waitingForPeer = { kind: "peerBusy", peer: "laptop-a" }
  record(retryLog, {})
  record(retryLog, { fleetRetry: { peer: 'laptop-a', at_ms: 4000 }, controlConnection: { stage: 'waiting' } })
  check('a scheduled fleet retry opens one live row', [retryLog.rows.length, retryLog.rows[0]?.retryAt, retryLog.rows[0]?.live], [1, 4000, true])
  retryLog.switchFailed = { kind: "switchFailed", device: "This laptop" }
  record(retryLog, { error: 'connection_failed' })
  check('a scheduled retry keeps the failed fleet transaction out of red errors',
    retryLog.rows.filter(row => row.kind === 'error'), [])
  const retryId = retryLog.rows[0]?.id
  record(retryLog, { fleetRetry: { peer: 'laptop-a', at_ms: 0 } })
  check('starting the fleet retry keeps its row', [retryLog.rows.length, retryLog.rows[0]?.id], [1, retryId])
  record(retryLog, { fleetRetry: {}, error: '', controlConnection: { stage: 'connecting' } })
  record(retryLog, { controlConnection: { stage: 'ready' } })
  check('the countdown becomes the successful connection in place',
    [retryLog.rows.length, retryLog.rows[0]?.message, retryLog.rows[0]?.live], [1, 'Connecting to AirPods... Connected!', false])

  check('a successful automatic retry never logs a terminal failure',
    retryLog.rows.filter(row => row.kind === 'error'), [])
  const exhaustedRoute = newLog()
  exhaustedRoute.switchFailed = { kind: "switchFailed", device: "This laptop" }
  record(exhaustedRoute, { fleetRetry: { peer: 'laptop-a', at_ms: 4000 }, controlConnection: { stage: 'waiting' } })
  record(exhaustedRoute, { fleetRetry: { peer: 'laptop-a', at_ms: 0, exhausted: true } })
  check('exhausting automatic retries reports the terminal fleet failure',
    exhaustedRoute.rows.filter(row => row.kind === 'error').map(row => row.message),
    ["This laptop couldn't connect to the AirPods."])

  for (const exhausted of [false, true]) {
    const stopped = newLog()
    record(stopped, { fleetRetry: { peer: 'laptop-a', at_ms: 4000 }, controlConnection: { stage: 'idle' } })
    record(stopped, { fleetRetry: exhausted ? { peer: 'laptop-a', at_ms: 0, exhausted: true } : {} })
    check('stopped retries stop animating', stopped.rows[0].live, false)
    check('stopped retry explains its outcome', stopped.rows[0].message,
      exhausted ? 'Waiting for laptop-a. Automatic retries stopped.' : 'AirPods connection retry cancelled.')
  }

  const cancelled = newLog()
  record(cancelled, { fleetRetry: { peer: 'laptop-a', at_ms: 4000 }, controlConnection: { stage: 'idle' } })
  record(cancelled, { fleetRetry: null, fleetRetryStop: { peer: 'laptop-a', reason: 'AirPods are no longer available nearby.' } })
  check('a stopped retry retains the attempt and explains why', lines(cancelled), [
    'Requested an AirPods connection via laptop-a.', 'Connection via laptop-a stopped: AirPods are no longer available nearby.'
  ])
  record(cancelled, {})
  check('a cancellation is not repeated on each state update', cancelled.rows.length, 2)

  // Resume reached control-ready before BlueZ refused the separate audio connection.
  for (const [reason, message] of [
    ["audio_profile_timeout", "audio connection timed out"],
    ["profile_error:org.bluez.Error.InProgress", "audio connection is still busy"],
    ["profile_error:org.bluez.Error.Failed", "audio connection failed"]
  ]) {
    const audioLog = newLog()
    record(audioLog, { controlConnection: { stage: "ready" }, localDevice: "test-laptop", error: reason })
    check("connected laptop is explicit for " + reason, lines(audioLog).at(-1), "test-laptop connected; " + message + ".")
  }
  const offlineAudioLog = newLog()
  record(offlineAudioLog, { controlConnection: { stage: "idle" }, error: "audio_profile_timeout" })
  check("an audio failure cannot claim a disconnected link", lines(offlineAudioLog), ["Audio connection timed out."])

  let log = newLog()
  record(log, { error: "playback_stopped" }); record(log, { error: "playback_paused" })
  check("a pause is not an error", lines(log), [])
  record(log, { error: "audio_operation_timeout:routing" })
  check("a real failure is logged", lines(log), ["Audio routing timed out."])

  log = newLog()
  report(log, "waiting", { kind: "unavailable", headset: false })
  check("waiting logs nothing", lines(log), [])
  report(log, "connecting", notHere)
  report(log, "handshake", notHere)
  report(log, "notifications", notHere)
  report(log, "ready", null)
  record(log, { refreshing: false, caption: inCase, inCaseOnly: true })
  check("an attempt that connects is one line", lines(log), ["Connecting to AirPods... Connected!"])

  log = newLog()
  report(log, "connecting", null)
  check("an attempt starts live", [lines(log), log.rows[0].live], [["Connecting to AirPods..."], true])
  report(log, "retrying", null)
  report(log, "handshake", null, true)
  check("a retry rewrites the attempt", lines(log), ["Connecting to AirPods... (Retrying 1 of 2)"])
  report(log, "ready", null); report(log, "ready", null)
  check("success completes the attempt once", [lines(log), log.rows[0].live], [["Connecting to AirPods... Connected!"], false])

  log = newLog()
  report(log, "retrying", notHere, true)
  report(log, "handshake", notHere, true)
  report(log, "ready", null, true)
  record(log, { refreshing: false, caption: appleKeepsSeat })
  check("recovery then the audio owner", lines(log),
    ["Connecting to AirPods... Connected!", "Apple fleet has the audio. Linux laptop keeps the Omarchy seat."])

  log = newLog()
  report(log, "connecting", null)
  record(log, { controlConnection: { stage: "failed", reason: "Connection timed out" } })
  // 2026-10-07 20:33: the failure sentence repeated the red reason line under it.
  check("a failed attempt stops and gives its reason once, in red", [lines(log), log.rows[0].live, log.rows[1].kind],
    [["Connecting to AirPods...", "Connection timed out"], false, "error"])
  record(log, { error: "Connection timed out" })
  check("the same reason as an error is not repeated", lines(log), ["Connecting to AirPods...", "Connection timed out"])
  log = newLog()
  report(log, "connecting", null)
  record(log, { controlConnection: { stage: "failed", reason: "Local AirPods reconnect timed out" }, error: "Local AirPods reconnect timed out" })
  check("a failure with its own error is one red line", lines(log), ["Connecting to AirPods...", "Local AirPods reconnect timed out"])

  for (const reason of ["Connection timed out", "BlueZ did not report the device as connected"]) {
    log = newLog()
    record(log, { suppress: true, controlConnection: { stage: "retrying", recovering: true } })
    record(log, { suppress: false, controlConnection: { stage: "failed", reason } })
    record(log, {})
    check("a hidden case recovery cannot start the log with a failure: " + reason, lines(log), [])
    report(log, "connecting", null)
    check("the first visible attempt is not called a retry: " + reason, lines(log), ["Connecting to AirPods..."])
    record(log, { controlConnection: { stage: "failed", reason } })
    check("a visible attempt after opening still reports its failure: " + reason, lines(log),
      ["Connecting to AirPods...", reason])

    log = newLog()
    record(log, { inCaseOnly: true, caption: inCase })
    record(log, { inCaseOnly: false, caption: null, controlConnection: { stage: "failed", reason } })
    check("docked availability also keeps recovery failure quiet: " + reason, lines(log), ["Available, in case"])

    log = newLog()
    record(log, { controlConnection: { stage: "failed", reason } })
    report(log, "idle", null)
    report(log, "connecting", null)
    check("a new attempt after failure announces recovery: " + reason, lines(log),
      [reason, "Trying to connect again...", "Connecting to AirPods..."])
    report(log, "handshake", null)
    report(log, "retrying", null)
    report(log, "ready", null)
    check("the recovery announcement stays separate from the completed attempt: " + reason, lines(log),
      [reason, "Trying to connect again...", "Connecting to AirPods... Connected!"])
    report(log, "idle", null)
    report(log, "connecting", null)
    check("success clears the previous failure: " + reason,
      lines(log).filter((line) => line === "Trying to connect again...").length, 1)
  }

  log = newLog()
  record(log, { controlConnection: { stage: "failed", reason: "Connection timed out" } })
  record(log, { suppress: true })
  record(log, { suppress: false, controlConnection: { stage: "connecting" } })
  check("clearing the log clears the recovery announcement", lines(log), ["Connecting to AirPods..."])

  // 2026-10-09: Apple Reconnect deliberately dropped the link while native audio preparation was running.
  log = newLog()
  record(log, { controlConnection: { stage: "idle", peer_reconnect: true }, error: "priority_connection_lost" })
  check("Apple reconnect does not report its intentional link loss", lines(log), ["Trying to reconnect the Apple device..."])
  record(log, { error: "Bluetooth disconnect failed" })
  check("a real disconnect failure during Apple reconnect remains visible", log.rows.at(-1).message, "Bluetooth disconnect failed")
  record(log, { controlConnection: { stage: "failed", peer_reconnect: false, reason: "Local AirPods reconnect timed out" },
    error: "Local AirPods reconnect timed out" })
  check("an Apple reconnect timeout remains visible", log.rows.at(-1).message, "Local AirPods reconnect timed out")
  log = newLog()
  record(log, { error: "priority_connection_lost" })
  check("an unexpected priority link loss remains visible", lines(log), ["priority_connection_lost"])

  // 2026-10-07 19:24: a lingering "Local AirPods reconnect timed out" came back each time a refused click's error passed.
  log = newLog()
  record(log, { error: "Local AirPods reconnect timed out" }); record(log, { error: "error: fleet routing in progress" })
  record(log, { error: "Local AirPods reconnect timed out" })
  check("a lingering error shows once", lines(log), ["Local AirPods reconnect timed out", "error: fleet routing in progress"])
  record(log, { error: "" }); record(log, { error: "Local AirPods reconnect timed out" })
  check("an error after the errors cleared shows again", lines(log).length, 3)

  log = newLog()
  record(log, { error: "ownership_changed" }); record(log, { audio: "AirPods audio moved to iPhone." })
  record(log, { error: "audio_profile_timeout" })
  check("an ownership change is not an error", lines(log), ["AirPods audio moved to iPhone.", "Audio connection timed out."])

  log = newLog()
  for (const stage of ["waiting", "ready"]) {
    log = newLog()
    report(log, "ready", null)
    record(log, { audio: "AirPods audio moved to Linux Laptop.", error: "An earlier connection failed." })
    report(log, stage, null)
    record(log, { suppress: true })
    record(log, { audio: "AirPods audio moved to iPhone." })
    check("a closed case clears the log: " + stage, lines(log), [])
    record(log, { suppress: false })
    report(log, "connecting", notHere); report(log, "ready", null)
    record(log, { audio: "AirPods audio moved to Linux Laptop." })
    check("a reopened case starts a new session: " + stage, lines(log),
      ["Connecting to AirPods... Connected!", "AirPods audio moved to Linux Laptop."])
  }
}

{
  // An open case with docked buds remains available during setup backoff.
  // Hiding that backoff used to end the live row and append another attempt.
  let log = null
  let t = 1000
  for (const [stage, retry, recovering] of [
    ['features', 1, false], ['retrying', 1, true], ['connecting', 2, true],
    ['handshake', 2, true], ['notifications', 2, true]
  ]) {
    log = Model.statusLog(log, { t, ev: 'status', status: {
      ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
      lidState: Model.LID_OPEN, caseLidState: Model.LID_OPEN, lidUpdatedMs: t,
      lastAdvertisementMs: t, caseConfirmedDocked: true,
      left: { ...Model.defaultPod(), level: 79, inCase: true },
      right: { ...Model.defaultPod(), level: 80, inCase: true },
      controlConnection: { stage, retry, recovering, retry_limit: 5 }
    } })
    check('open-case setup keeps one connection row: ' + stage, log.rows.length, 1)
    check('open-case setup keeps its row live: ' + stage, log.rows[0].live, true)
    t += 1000
  }
  check('open-case retry retains the count', log.rows[0].detail, '(Retrying 2 of 5)')
}




// A giving laptop knows a switch is underway even without a remembered seat map.
{
  const local = '02:00:00:00:00:03', gram = '02:00:00:00:00:01', phone = '02:00:00:00:00:04'
  const status = { ...Model.defaultStatus(), ok: true, daemonReachable: true,
    localAddress: local, localName: 'omarchy-xps', connected: false, bluezConnected: false,
    devices: { [gram]: { kind: 'omarchy', label: 'lg-gram' }, [phone]: { kind: 'apple', label: 'iPhone' } },
    controlConnection: { stage: 'interrupted', recovering: true }, waitingForPlayback: true,
    fleet: { routing: { phase: 'waiting_connection', busy: true, peer: gram, retain: phone } } }
  const view = Model.seatPresentation(status, false, false)
  check('an admitted handoff shows Switching without seat memory', view.omarchyTransitionLabel, 'Switching…')
  check('the departing XPS leaves its seat during an admitted handoff', view.seats.omarchy, [])
  const roster = { reporter: gram, peers: [gram, phone], pending: false, audio_source: phone }
  for (const bluezConnected of [false, true]) {
    const landed = Model.seatPresentation({ ...status, bluezConnected,
      fleet: { ...status.fleet, roster } }, false, false)
    check('a confirmed Gram roster seats Gram even before BlueZ finishes disconnecting',
      landed.seats.omarchy.map(d => d.label), ['lg-gram'])
    check('a confirmed Gram roster clears Switching before the completion reply', landed.omarchyTransitionLabel, '')
  }
  const expired = Model.seatPresentation({ ...status,
    fleet: { routing: { phase: 'failed', busy: false, peer: gram, retain: phone } } }, false, false)
  check('an ended handoff leaves the empty seat unlabelled', expired.omarchyTransitionLabel, '')
}

// The admission keeps the playing iPhone while only the local laptop leaves.
{
  const local = '02:00:00:00:00:03', gram = '02:00:00:00:00:01'
  const phone = '02:00:00:00:00:04', mac = '02:00:00:00:00:05'
  const initial = { ...Model.defaultStatus(), ok: true, connected: true, bluezConnected: true,
    localAddress: local, localName: 'omarchy-xps', connectedPeers: [phone, local], peerListUpdated: 1000,
    controlConnection: { stage: 'ready' }, audioOwner: { holder: 'remote', playing: true },
    remoteSourceAddress: phone, rosterAudioOwner: phone,
    devices: { [gram]: { label: 'lg-gram', kind: 'omarchy' }, [phone]: { label: 'iPhone', kind: 'apple' },
      [mac]: { label: 'Mac', kind: 'apple' } } }
  let state = Model.statusLog(null, { t: 1000, ev: 'status', status: initial })
  const giving = { ...initial, connected: false, connectedPeers: [], remoteSourceAddress: '',
    controlConnection: { stage: 'interrupted', recovering: true },
    fleet: { routing: { phase: 'waiting_connection', busy: true, peer: gram, retain: phone } } }
  for (const [t, bluezConnected] of [[2000, true], [3000, false], [5000, false]]) {
    state = Model.statusLog(state, { t, ev: 'status', status: { ...giving, bluezConnected } })
    check('iPhone stays seated while XPS leaves at ' + t,
      state.seat.seats.apple.map(d => d.address), [phone])
    check('the retained iPhone keeps its playback glow at ' + t,
      [state.seat.appleOwnsAudio, state.seat.audioPlaying, state.seat.audioDeviceAddress], [true, true, phone])
    check('only XPS goes to the tray at ' + t, state.seat.seats.omarchy, [])
  }
  // Gram can confirm the same iPhone, replace it with Mac, or confirm an empty Apple seat.
  for (const apple of [phone, mac, '']) {
    const roster = { reporter: gram, peers: apple ? [gram, apple] : [gram], pending: false, audio_source: apple }
    const confirmed = Model.statusLog(state, { t: 6000, ev: 'status', status: {
      ...giving, bluezConnected: false, fleet: { ...giving.fleet, roster }
    } })
    check('Gram confirms the Apple seat: ' + apple,
      confirmed.seat.seats.apple.map(d => d.address), apple ? [apple] : [])
    check('Gram joins without restoring XPS', confirmed.seat.seats.omarchy.map(d => d.address), [gram])
  }
}

// A laptop handoff changes connections, not the retained phone's audio ownership.
{
  const local = '02:00:00:00:00:03', gram = '02:00:00:00:00:01', phone = '02:00:00:00:00:04'
  const base = { ...Model.defaultStatus(), ok: true, localAddress: local, localName: 'omarchy-xps',
    connected: true, bluezConnected: true, peerListUpdated: 1000, connectedPeers: [local, phone],
    left: { ...Model.defaultPod(), inEar: true }, audioOwner: REMOTE_PLAYING, remoteSourceAddress: phone,
    controlConnection: { stage: 'ready' }, nativeHandoff: { audio_output: 'speakers', audio_active: false },
    devices: { [gram]: { label: 'lg-gram', kind: 'omarchy' }, [phone]: { label: 'iPhone', kind: 'apple' } } }
  let state = Model.statusLog(null, { t: 1000, ev: 'status', status: base })
  const start = state.rows.length
  const give = { ...base, connected: false, connectedPeers: [], remoteSourceAddress: '',
    audioOwner: UNKNOWN, waitingForPlayback: true,
    controlConnection: { stage: 'interrupted', reason: 'link_lost', recovering: true },
    fleet: { routing: { phase: 'waiting_connection', busy: true, peer: gram, retain: phone } } }
  const roster = { reporter: gram, peers: [gram, phone], audio_source: phone, pending: false }
  const done = { ...give, bluezConnected: false,
    fleet: { routing: { ...give.fleet.routing, phase: 'success', busy: false }, roster } }
  for (const [t, status] of [[2000, give], [2500, { ...give, bluezConnected: false }]]) {
    state = Model.statusLog(state, { t, ev: 'status', status })
    check('a departing XPS does not log Gram as seated before its arrival',
      state.rows.slice(start).some(r => r.event?.kind === 'fleetSwitched'), false)
  }
  const completedBeforeRoster = { ...done, fleet: { routing: done.fleet.routing } }
  state = Model.statusLog(state, { t: 2800, ev: 'status', status: completedBeforeRoster })
  check('completion without a seat report does not announce the animation early',
    state.rows.slice(start).some(r => r.event?.kind === 'fleetSwitched'), false)
  state = Model.statusLog(state, { t: 3000, ev: 'status', status: done })
  check('the confirmed roster starts the arrival and completion log together',
    [state.seat.seats.omarchy.map(d => d.address),
      state.rows.filter(r => r.event?.kind === 'fleetSwitched').map(r => r.at)], [[gram], [3000]])
  state = Model.statusLog(state, { t: 7000, ev: 'status', status: done })
  check('yielding to Gram has no reconnect instructions', state.rows.slice(start).filter(r =>
    r.event?.kind === 'waitingForPlayback' || r.event?.connection?.stage === 'interrupted').length, 0)
  check('yielding records one completed seat switch', state.rows.slice(start).filter(r =>
    r.event?.kind === 'fleetSwitched' && r.event.device === 'lg-gram').length, 1)
  const join = { ...done, waitingForPlayback: false,
    fleet: { roster, routing: { phase: 'connecting', busy: true, peer: gram, retain: phone } },
    controlConnection: { stage: 'connecting' } }
  for (const [t, status] of [[8000, join], [9000, { ...join, connected: true, bluezConnected: true,
    controlConnection: { stage: 'notifications' }, audioOwner: REMOTE_PLAYING }],
    [10000, { ...base, fleet: { routing: { phase: 'success', busy: false, peer: gram, retain: phone } } }]])
    state = Model.statusLog(state, { t, ev: 'status', status })
  check('joining beside the same iPhone logs no audio takeover', state.rows.slice(start).filter(r => r.event?.kind === 'audioTaken'), [])
  // A later unplanned loss must still explain how to reconnect.
  state = Model.statusLog(state, { t: 11000, ev: 'status', status: {
    ...give, bluezConnected: false, fleet: { routing: { phase: 'success', busy: false, peer: gram, retain: phone } }
  } })
  check('a later real disconnect logs instructions exactly once', state.rows.filter(r => r.event?.connection?.stage === 'interrupted').length, 1)
  check('an old successful route cannot turn Apple displacement into Switching',
    [state.seat.switching, state.seat.omarchyTransitionLabel], [false, ''])
  check('after Apple displacement the former iPhone seat is unknown',
    [state.seat.appleSeatUnknown, state.seat.seats.apple.length, state.seat.seats.omarchy.length], [true, 0, 0])
  state = Model.statusLog(state, { t: 12000, ev: 'tick' })
  check('the next refresh does not resurrect the old handover', state.seat.switching, false)


  // Gram was admitted alongside XPS, but the pods kept iPhone and dropped XPS instead.
  state = Model.statusLog(null, { t: 20000, ev: 'status', status: base })
  const alongside = { ...give.fleet.routing, retain: local }
  state = Model.statusLog(state, { t: 21000, ev: 'status', status: {
    ...base, fleet: { routing: alongside }
  } })
  for (const [t, status] of [[22000, { ...give, fleet: { routing: alongside } }],
    [23000, { ...give, bluezConnected: false, fleet: { routing: alongside,
      roster: { ...roster, peers: [gram, local], audio_source: '', pending: true } } }],
    [24000, { ...done, fleet: { routing: alongside, roster } }],
    [25000, { ...done, fleet: { routing: { ...alongside, busy: false, phase: 'success' }, roster } }],
    [30000, { ...done, fleet: { routing: { ...alongside, busy: false, phase: 'success' }, roster } }]])
    state = Model.statusLog(state, { t, ev: 'status', status })
  check('an admission that displaces the retained XPS is still a handoff, not a reconnect prompt',
    state.rows.filter(r => r.event?.kind === 'waitingForPlayback' || r.event?.connection?.stage === 'interrupted'), [])
}

{
  const local = '02:00:00:00:00:03', gram = '02:00:00:00:00:01'
  const ready = { ...Model.defaultStatus(), ok: true, localAddress: local, localName: 'omarchy-xps',
    connected: true, bluezConnected: true, connectedPeers: [local, gram], peerListUpdated: 1000,
    left: { ...Model.defaultPod(), inEar: true }, audioOwner: LOCAL_PLAYING,
    controlConnection: { stage: 'ready' }, devices: { [gram]: { label: 'lg-gram', kind: 'omarchy' } } }
  let state = Model.statusLog(null, { t: 1000, ev: 'status', status: ready })
  const reconnect = { ...ready, connected: false, connectedPeers: [], wantOmarchySeat: true,
    ownershipIntent: 'claim', holdingOmarchySeat: true,
    controlConnection: { stage: 'connecting', peer_reconnect: true },
    nativeHandoff: { phase: 'reconnecting', connecting: true, updated_ms: 2000 } }
  for (const bluezConnected of [true, false]) {
    state = Model.statusLog(state, { t: bluezConnected ? 2000 : 3000, ev: 'status', status: { ...reconnect, bluezConnected } })
    check('Apple reconnect only removes the local link before peer confirmation: ' + bluezConnected,
      state.seat.seats.omarchy.map(d => d.address), [gram])
  }
  state = Model.statusLog(state, { t: 4000, ev: 'status', status: { ...reconnect, bluezConnected: false,
    fleet: { roster: { reporter: gram, peers: [gram], pending: false, audio_source: gram } } } })
  check('Gram fleet confirmation keeps it seated during Apple reconnect', state.seat.seats.omarchy.map(d => d.address), [gram])
  check('the Omarchy seat waits during an Apple reconnect', state.seat.omarchyTransitionLabel, 'Waiting…')
}

// 2026-10-12: XPS joined beside a playing iPhone. Its socket opened before the pods listed their peers, the Gram's
// roster time stood in for that list, and the iPhone left the Apple seat while the panel read "Connecting".
{
  const gram = "02:00:00:00:00:01", phone = "02:00:00:00:00:02", xps = "02:00:00:00:00:03"
  const base = { ...Model.defaultStatus(), daemonReachable: true, localAddress: xps, localName: "omarchy-xps",
    devices: { [gram]: { label: "lg-gram", kind: "omarchy" }, [phone]: { label: "iPhone", kind: "apple" },
      [xps]: { label: "omarchy-xps", kind: "omarchy", nearby: true } },
    audioOwner: { holder: "", playing: false } }
  const roster = { reporter: gram, peers: [gram, phone], audio_source: phone, sent_ms: 5000 }
  const observing = { ...base, peerListUpdated: 5000, fleet: { roster } }
  const routing = { phase: "waiting_completion", busy: true, peer: gram, retain: phone, admission: "ok" }
  const joining = { ...base, peerListUpdated: 5000, controlConnection: { stage: "connecting" },
    fleet: { roster, routing } }
  // The Gram has let go, so its next roster is empty; the XPS socket is up, setup unfinished, and no pod list yet.
  const opened = { ...base, connected: true, bluezConnected: true,
    controlConnection: { stage: "handshake" }, connectedPeers: [], peerListUpdated: 0,
    fleet: { roster: { ...roster, peers: [], audio_source: "", sent_ms: 5200 }, routing } }
  let presentation = null
  const steps = [[observing, 1000], [joining, 2000], [opened, 3000]]
  const seats = steps.map(([status, now]) => {
    presentation = Model.connectionPresentation(status, presentation, now)
    const view = Model.seatPresentation({ ...status, connectionPresentation: presentation }, false, false)
    return [view.seats.apple.map((d) => d.label), view.takingOverOmarchy]
  })
  check("a joining laptop keeps the retained iPhone seated until the pods list their peers",
    seats.map((s) => s[0]), [["iPhone"], ["iPhone"], ["iPhone"]])
  check("taking the Gram's seat is a seat switch, drawn without a trace", seats.map((s) => s[1]), [false, true, true])
  // The Gram's roster may name no source, or an idle phone. Asking for its seat is a switch either way.
  for (const [audio_source, audio_idle] of [["", false], [phone, true]]) {
    const quiet = { ...roster, audio_source, audio_idle }
    const asking = { ...base, peerListUpdated: 5000,
      fleet: { roster: quiet, routing: { phase: "waiting_admission", busy: true, peer: gram } } }
    const view = Model.seatPresentation({ ...asking,
      connectionPresentation: Model.connectionPresentation(asking, null, 1000) }, false, false)
    check("asking for the seat is a switch, source=" + (audio_source || "none") + " idle=" + audio_idle,
      [view.takingOverOmarchy, view.omarchyTransitionLabel], [true, audio_idle ? "" : "Switching…"])
  }
}

// 2026-10-12: device names in the activity log read like the rest of the message.
{
  const names = Model.logDeviceNames({ a: { label: "lg-gram" }, b: { label: "iPhone" }, c: { label: "Mac" },
    d: { label: "MacBook Pro" } }, "omarchy-xps")
  check("the log knows this laptop and every device", names, ["omarchy-xps", "lg-gram", "iPhone", "Mac", "MacBook Pro"])
  const mark = (text) => Model.deviceNameMarkup(text, names, "#7aa2f7")
  const name = (n) => '<font color="#7aa2f7">' + n + "</font>"
  check("a name is coloured", mark("AirPods audio moved to omarchy-xps."), "AirPods audio moved to " + name("omarchy-xps") + ".")
  check("a name before a full stop", mark("Omarchy Fleet connection switched to lg-gram."),
    "Omarchy Fleet connection switched to " + name("lg-gram") + ".")
  check("the longest name wins", mark("MacBook Pro connected alongside Mac."),
    name("MacBook Pro") + " connected alongside " + name("Mac") + ".")
  check("a name inside a longer word is left alone", mark("Machine and lg-gram-2"), "Machine and lg-gram-2")
  check("markup in a message is escaped", Model.deviceNameMarkup("<b>lg-gram</b> & co", ["lg-gram"], "#fff"),
    "&lt;b&gt;" + '<font color="#fff">lg-gram</font>' + "&lt;/b&gt; &amp; co")
  check("no names leaves the text", Model.deviceNameMarkup("Connected to AirPods", [], "#fff"), "Connected to AirPods")
}



check('multiple pairing candidates direct selection to Bluetooth', Model.emptyGuidance(true, false, 11, true, true, 2), 'More than one pair of AirPods is ready to pair. Select your AirPods in the Bluetooth plugin.')
check('pairing candidate count comes from daemon', Model.parseStatus('{"schema_version":1,"pairing_candidates":2}').pairingCandidates, 2)

for (const model of [1, 2, 3, 4, 5, 6, 9, 10, 11]) {
  check('closed or missing case evidence prompts preparation ' + model,
    Model.emptyGuidance(true, false, model, true, false, 0, false),
    'Open the case lid with both AirPods inside.')
  check('open docked case uses the model gesture ' + model,
    Model.emptyGuidance(true, false, model, true, false, 0, true).startsWith(model >= 9 ? 'Double-tap' : 'Hold the button'), true)
}
check('stale presence cannot advance setup', Model.emptyGuidance(true, false, 11, false, false, 0, true), 'Open the case lid with both AirPods inside.')

{
  const worn = { inEar: true, inCase: false }
  check('unpaired artwork defaults to closed despite unidentified worn reports',
    Model.caseIconState(false, Model.LID_UNKNOWN, worn, worn, '', true, 1000, 1100, 20000, false, true, true), 'closed')
  check('paired worn artwork remains buds out',
    Model.caseIconState(false, Model.LID_UNKNOWN, worn, worn, '', true, 1000, 1100, 20000, true, true, false), '')
  const docked = { inEar: false, inCase: true }
  check('unpaired open case report still shows open artwork',
    Model.caseIconState(false, Model.LID_OPEN, docked, docked, 'closed', false, 1000, 1100, 20000, false, true, true), 'open')
}

const enrolledPeer = Model.devicesMap([{ address: "AA:BB:CC:DD:EE:FF", label: "gram", kind: "omarchy", discovered: true, fleet_enrolled: true }]);
check('enrollment hint reaches tray peer', Model.availableDevices("omarchy", [], enrolledPeer)[0].fleetEnrolled, true);
check('public enrollment hint remains discovery only', Model.availableDevices("omarchy", [], enrolledPeer)[0].discovered, true);

if (failures > 0) {
  console.log(failures + " failed")
  Deno.exit(1)
}
console.log("model.test.js: all checks passed")

// A temporary Bluetooth connection must not masquerade as saved pairing.
for (const bonded of [true, false, null]) {
  const status = Model.parseStatus(JSON.stringify({schema_version: 1,
    bluetooth_paired: true, bluetooth_bonded: bonded, pairing_keys_ready: true}))
  status.daemonReachable = true
  check('bond state remains distinct from AirPods keys: ' + bonded, status.bluetoothBonded, bonded)
  check('only an explicitly unsaved bond warns: ' + bonded,
    Model.pairingWarning(status).includes('Pairing hasn’t been saved'), bonded === false)
  status.bluetoothPaired = false
  check('unpaired setup keeps ordinary instructions: ' + bonded, Model.pairingWarning(status), '')
}
check('old daemons do not invent missing bonds', Model.parseStatus('{"schema_version":1}').bluetoothBonded, null)
