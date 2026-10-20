import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import "Model.js" as Model

Item {
  id: root
  Component.onCompleted: console.log("fleetpods-activity policy=fresh-input-only")

  property var settings: ({})
  readonly property bool fleetBusy: fleetProcess.running
  property string fleetRequestError: ""

  signal fleetRequestFinished(bool ok, string error)

  function fleetCommand(request) {
    if (fleetProcess.running) return
    fleetRequestError = ""
    fleetProcess.request = JSON.stringify(request)
    fleetProcess.running = true
  }

  Process {
    id: fleetProcess
    property string request: ""
    command: [root.ctlPath, "fleet"]
    stdinEnabled: true
    onStarted: { write(request + "\n"); request = "" }
    stdout: StdioCollector { id: fleetReply; waitForEnd: true }
    stderr: StdioCollector { id: fleetError; waitForEnd: true }
    onExited: function(exitCode) {
      var reply = (fleetReply.text || fleetError.text).trim()
      var ok = exitCode === 0 && reply.indexOf("error:") !== 0
      if (!ok) root.fleetRequestError = reply.replace(/^error:\s*/, "") || "Fleet request failed. Check Bluetooth and try again."
      root.fleetRequestFinished(ok, root.fleetRequestError)
      root.refresh()
    }
  }

  IdleMonitor {
    id: desk
    timeout: 2
    respectInhibitors: false
    onIsIdleChanged: console.log("fleetpods-activity idle=" + isIdle)
  }

  ActivityGate {
    id: activityGate
    idle: desk.isIdle
    onActiveChanged: if (active && !screensaverCheck.running) screensaverCheck.running = true
  }

  // The compositor can report input while Omarchy's screensaver is up without
  // anyone at the desk (gram, 2026-10-19: a resume 0.3 s after the screensaver
  // cycle started took the pods from a playing iPhone). Someone who really
  // comes back closes the screensaver: it exits on a key press, or within about
  // a second of losing focus. So a resume counts only if no screensaver is up,
  // or if it closes within 3 s.
  Process {
    id: screensaverCheck
    command: ["sh", "-c",
      "up() { hyprctl clients -j | grep -q '\"class\": \"org.omarchy.screensaver\"'; }; "
      + "up || exit 0; for i in $(seq 12); do sleep 0.25; up || exit 0; done; exit 1"]
    onExited: function(exitCode) {
      if (exitCode === 0) root.requestActivity("desktop-resumed")
      else console.log("fleetpods-activity ignored: the screensaver is still up")
    }
  }

  // The control reply returns before the link is up, and the socket has 8s to finish.
  // "accepted" keeps this hold through that window. "ignored" clears it.
  readonly property int activityHoldMs: 9000
  property bool activityHold: false
  // A refused route fails within a second; the daemon paces the retry, so the hold must not.
  property real activityRequestedMs: 0
  property real routeFailedMs: 0
  // The status log and the panel's status view (Model.statusLog), stepped on every status, command and seat click,
  // and woken when one of its held lines or debounces expires.
  property var statusLogState: Model.statusLogState()
  function stepStatusLog(event) {
    event.t = Date.now()
    statusLogState = Model.statusLog(statusLogState, event)
    if (statusLogState.wakeAt > 0) {
      statusLogWake.interval = Math.max(1, statusLogState.wakeAt - event.t)
      statusLogWake.restart()
    } else statusLogWake.stop()
  }
  function noteSeatClick() {
    stepStatusLog({ ev: "click" })
  }
  Timer {
    id: statusLogWake
    repeat: false
    onTriggered: root.stepStatusLog({ ev: "tick" })
  }

  function requestActivity(source) {
    if (!daemonReachable || busy || activityHold || activityProcess.running)
      return
    console.log("fleetpods-activity request source=" + String(source || "unspecified")
      + " connected=" + connected + " idle=" + desk.isIdle)
    activityHold = !connected
    activityRequestedMs = Date.now()
    activityProcess.running = true
  }

  function routeFailedSinceRequest() {
    return activityRequestedMs > 0 && routeFailedMs >= activityRequestedMs
  }

  function releaseActivityHold() {
    activitySettle.stop()
    activityHold = false
  }

  Process {
    id: activityProcess
    command: [root.ctlPath, "activity"]
    stdout: StdioCollector { id: activityOut; waitForEnd: true }
    onExited: function (exitCode) {
      if (root.connected) {
        root.releaseActivityHold()
        return
      }
      var reply = String(activityOut.text || "").trim()
      var last = reply.length ? reply.split("\n").pop().trim() : ""
      if (exitCode === 0 && last !== "ignored" && !root.routeFailedSinceRequest()) {
        root.activityHold = true
        activitySettle.restart()
        return
      }
      root.releaseActivityHold()
    }
  }

  Timer {
    id: activitySettle
    interval: root.activityHoldMs
    onTriggered: root.activityHold = false
  }

  // The status as the panel reads it, from the status log; this service keeps only commands, activity, optimistic
  // control values and the case icon.
  readonly property var view: statusLogState.view
  readonly property bool daemonReachable: view.daemonReachable
  readonly property bool connected: view.connected
  property int noiseMode: Model.NOISE_UNKNOWN
  property int adaptiveNoiseLevel: 0
  property bool oneBudANC: false
  property bool conversationalAwareness: false
  property int earDetectionBehavior: Model.EAR_PAUSE_ONE_OUT
  property string caseIconState: "closed"
  property bool lastAnyInEar: false
  property var rememberedPlacement: ({ left: null, right: null })
  // Match the pod lift; the artwork itself waits for both pods to clear.
  readonly property int emptyCaseHoldMs: 840
  readonly property int emptyCaseConfirmMs: 250
  property bool emptyCasePending: false
  property bool schemaUnsupported: false

  readonly property string ctlPath: String(setting("ctlPath", "") || "fleetpods")
  readonly property bool busy: commandProcess.running
  // The daemon publishes here on change, so there is nothing to poll.
  readonly property string statePath: (Quickshell.env("XDG_STATE_HOME")
    || Quickshell.env("HOME") + "/.local/state") + "/fleetpods/status.json"
  onConnectedChanged: if (connected) releaseActivityHold()

  // How long an optimistic value is held before the daemon's own state wins.
  readonly property int settleHoldMs: 4000

  // Held over incoming reads until the daemon agrees, so a write already in flight
  // when the click landed cannot snap the control back.
  property string _pendingField: ""
  property var _pendingValue: null

  // Single slot: a verb sent while another is in flight replaces the queued one
  // rather than being dropped, which is what arrow-key repeat produces.
  property var _queued: null

  function setting(name, fallback) {
    var value = settings ? settings[name] : undefined
    return value === undefined || value === null ? fallback : value
  }

  function refresh() {
    stateFile.reload()
  }

  function applyLine(raw) {
    var status = Model.parseStatus(raw)
    stepStatusLog({ ev: "status", status: status })
    schemaUnsupported = !status.ok && status.schemaTooNew
    if (!status.ok) {
      resetConnectionState()
      return
    }
    applyStatus(status)
  }

  // The daemon removes the file when it stops, so an absent file is a stopped daemon.
  function resetConnectionState() {
    emptyCaseConfirmTimer.stop()
    emptyCasePending = false
    emptyCaseTimer.stop()
    caseIconState = "closed"
    lastAnyInEar = false
    rememberedPlacement = { left: null, right: null }
    _queued = null
    _clearPending()
  }

  function stateGone() {
    resetConnectionState()
    schemaUnsupported = false
    stepStatusLog({ ev: "gone" })
  }

  // Runs after the status log has taken the line, so view is this status.
  function applyStatus(status) {
    rememberedPlacement = status.bluetoothPaired === false ? { left: null, right: null } : {
      left: Model.rememberPodPlacement(status.left, rememberedPlacement.left),
      right: Model.rememberPodPlacement(status.right, rememberedPlacement.right)
    }
    if (Model.fleetRouteFailed(statusLogState.routeEnded)) {
      routeFailedMs = Date.now()
      if (activityHold && routeFailedSinceRequest()) releaseActivityHold()
    }
    tickCaseIcon()
    if (status.earReported) lastAnyInEar = status.left.inEar || status.right.inEar
    noiseMode = _settle("noiseMode", status.noiseMode)
    adaptiveNoiseLevel = _settle("adaptiveNoiseLevel", status.adaptiveNoiseLevel)
    oneBudANC = _settle("oneBudANC", status.oneBudANC)
    conversationalAwareness = _settle("conversationalAwareness", status.conversationalAwareness)
    earDetectionBehavior = _settle("earDetectionBehavior", status.earDetectionBehavior)
  }

  function _settle(field, reported) {
    if (_pendingField !== field) return reported
    if (reported === _pendingValue) {
      _clearPending()
      return reported
    }
    return _pendingValue
  }

  function _clearPending() {
    _pendingField = ""
    _pendingValue = null
    settleTimer.stop()
  }

  function _send(verb, field, optimistic) {
    if (verb === "") return
    stepStatusLog({ ev: "send", verb: verb })
    if (commandProcess.running) {
      _queued = { verb: verb, field: field, optimistic: optimistic }
      if (field) {
        _pendingField = field
        _pendingValue = optimistic
        root[field] = optimistic
      }
      settleTimer.restart()
      return
    }
    if (field) {
      _pendingField = field
      _pendingValue = optimistic
      root[field] = optimistic
    }
    settleTimer.restart()
    commandProcess.command = [ctlPath, verb]
    commandProcess.running = true
  }

  // Guards the keyboard and the bar's right click too, not just the panel rows.
  function setNoiseMode(mode) {
    if (availableModes().indexOf(mode) < 0) return
    _send(Model.noiseModeVerb(mode), "noiseMode", mode)
  }

  function availableModes() {
    return Model.availableModes(view.supportsNoiseControl, view.supportsNoiseOff, view.supportsAdaptive)
  }

  function cycleNoiseMode() {
    if (!view.hasAirPods) return
    var modes = availableModes()
    if (modes.length === 0) return
    var at = modes.indexOf(noiseMode)
    // An unknown current mode has no next one, so start at the head instead of past it.
    setNoiseMode(at < 0 ? modes[0] : modes[(at + 1) % modes.length])
  }

  function setAdaptiveNoiseLevel(level) {
    var clamped = Math.max(0, Math.min(100, Math.round(level)))
    _send("adaptive:" + clamped, "adaptiveNoiseLevel", clamped)
  }

  function setConversationalAwareness(enabled) {
    _send(enabled ? "ca:on" : "ca:off", "conversationalAwareness", enabled)
  }

  function setOneBudANC(enabled) {
    _send(enabled ? "onebud:on" : "onebud:off", "oneBudANC", enabled)
  }

  function setEarDetectionBehavior(behavior) {
    _send(Model.earDetectionVerb(behavior), "earDetectionBehavior", behavior)
  }

  function cycleEarDetection() {
    setEarDetectionBehavior((earDetectionBehavior + 1) % Model.EAR_BEHAVIOR_COUNT)
  }

  // Daemon timestamps are wall-clock ms. Tests use small counters.
  function caseIconNowMs(advertisementMs) {
    return Number(advertisementMs || 0) > 1000000000000 ? Date.now() : 0
  }

  function tickCaseIcon() {
    if (view.pairingMode && (!view.bluetoothPaired || !view.pairingKeysReady)) {
      applyCaseIcon("open")
      return
    }
    applyCaseIcon(Model.caseIconState(view.isHeadset, view.iconLidState,
      view.left, view.right, caseIconState, lastAnyInEar,
      view.lastAdvertisementMs, caseIconNowMs(view.lastAdvertisementMs), 20000, view.connected, view.earReported,
      (!view.bluetoothPaired || !view.pairingKeysReady), rememberedPlacement))
  }

  function applyCaseIcon(nextCase) {
    if (nextCase === "open-empty") {
      // Conflicting ear reports must settle before showing the empty-case animation.
      if (caseIconState !== "open-empty" && !emptyCasePending) {
        emptyCasePending = true
        emptyCaseConfirmTimer.restart()
      }
      return
    }
    emptyCaseConfirmTimer.stop()
    emptyCasePending = false
    emptyCaseTimer.stop()
    caseIconState = nextCase
  }

  function confirmEmptyCase() {
    emptyCaseConfirmTimer.stop()
    if (!emptyCasePending) return
    emptyCasePending = false
    caseIconState = "open-empty"
    emptyCaseTimer.restart()
  }

  function emptyCaseHoldDone() {
    emptyCaseTimer.stop()
    if (caseIconState === "open-empty") caseIconState = ""
  }

  // The status log holds the optimistic claim and release from the send event.
  function claimOwnership() {
    _send("connect")
  }

  function reconnectPeer() {
    _send("reconnect-peer")
  }

  function releaseOwnership() {
    _send("handoff:release")
  }

  Timer {
    // Bounds the optimistic hold, and re-reads because a verb that changed nothing
    // leaves the daemon's file untouched, so no watch fires to correct the display.
    id: settleTimer
    interval: root.settleHoldMs
    repeat: false
    onTriggered: { root._clearPending(); root.refresh() }
  }

  Timer {
    id: emptyCaseConfirmTimer
    interval: root.emptyCaseConfirmMs
    repeat: false
    onTriggered: root.confirmEmptyCase()
  }

  Timer {
    id: emptyCaseTimer
    interval: root.emptyCaseHoldMs
    repeat: false
    onTriggered: root.emptyCaseHoldDone()
  }

  FileView {
    id: stateFile
    path: root.statePath
    watchChanges: true
    printErrors: false
    // text() is stale inside the change signal, so both paths go through reload.
    onFileChanged: reload()
    onLoaded: root.applyLine(text())
    onLoadFailed: root.stateGone()
  }

  Process {
    id: commandProcess
    running: false
    command: []
    stderr: StdioCollector { id: commandErr; waitForEnd: true }
    onExited: function (exitCode) {
      if (exitCode !== 0) {
        // Clearing the hold also stops the timer that would have re-read, so do it here.
        root._clearPending()
        root.refresh()
        root._queued = null
        root.stepStatusLog({ ev: "reply", error: Model.elideError(commandErr.text || "fleetpods rejected the command") })
      }
      if (root._queued) {
        var next = root._queued
        root._queued = null
        root._send(next.verb, next.field, next.optimistic)
      }
    }
  }
}
