import QtQuick
import QtTest
import ".." as Pods
import "../Model.js" as Model

TestCase {
  name: "ReconnectSeat"
  when: windowShown
  visible: true
  width: 440
  height: 400

  Component { id: view; Pods.SeatsView { width: 440; height: implicitHeight } }
  SignalSpy { id: reconnect; signalName: "reconnectClicked" }
  SignalSpy { id: release; signalName: "appleClicked" }
  SignalSpy { id: enroll; signalName: "enrollClicked" }

  function test_caseExitRevealsRestingPodsConnectors() {
    var item = createTemporaryObject(view, this, { podsVariant: "pro", caseState: "open-empty" })
    var art = findChild(item, "seatPodsArtwork")
    item.caseState = ""
    verify(art.caseExiting)
    var target = item.connectorEndY
    verify(target > item.revealedConnectorEndY, "case initially covers the lower wire")
    verify(item.revealedConnectorEndY > item.linkY + 8, "both wire ends point down")
    wait(100)
    compare(item.connectorEndY, target, "target stays in the final pods position")
    compare(item.revealedConnectorEndY, target, "case descent uncovers the full wire")
    tryCompare(art, "caseExiting", false, 600)
    fuzzyCompare(item.connectorEndY, target, 0.001, "renderer swap must not move the connectors")
    wait(100)
    fuzzyCompare(item.revealedConnectorEndY, target, 0.001, "pods enter over the fixed wires")
  }

  function test_connectedDiscoveryStaysInTrayWithInvitation() {
    var names = { "02:00:00:00:00:04": { label: "XPS", kind: "omarchy", discovered: true } }
    var peers = ["02:00:00:00:00:01", "02:00:00:00:00:04"]
    var seats = Model.seatOccupants(peers, "02:00:00:00:00:01", "Gram", names, true)
    var waiting = Model.availableDevices("omarchy", peers, names, "02:00:00:00:00:01")
    var item = createTemporaryObject(view, this, {
      appleDevices: seats.apple, omarchyDevices: seats.omarchy,
      availableOmarchy: waiting, connectedToOmarchy: true
    })
    compare(seats.apple.length, 0)
    compare(seats.omarchy.length, 1)
    var button = findChild(item, "fleetEnrollButton")
    verify(button.visible)
    enroll.target = item; enroll.clear()
    wait(500)
    mouseClick(button)
    compare(enroll.count, 1)
    compare(enroll.signalArguments[0][0], "02:00:00:00:00:04")
    compare(enroll.signalArguments[0][1], "XPS")
  }

  function test_keyringWaitHidesEnrollmentUntilMembershipIsKnown_data() {
    return [{ tag: "join", enrolled: true }, { tag: "invite", enrolled: false }]
  }

  function test_keyringWaitHidesEnrollmentUntilMembershipIsKnown(data) {
    var item = createTemporaryObject(view, this, {
      availableOmarchy: [{ address: "02:00:00:00:00:04", label: "Gram", kind: "omarchy",
        discovered: true, fleetEnrolled: data.enrolled }],
      fleetWaitingForKeyring: true
    })
    var button = findChild(item, "fleetEnrollButton")
    verify(button !== null)
    verify(!button.visible, "unknown membership must not offer re-enrollment")
    item.fleetWaitingForKeyring = false
    verify(button.visible, "a discovered peer can be enrolled after a successful read")
    compare(button.text, data.enrolled ? "Join fleet" : "+ Add to fleet")
    item.availableOmarchy = [{ address: "02:00:00:00:00:04", label: "Gram", kind: "omarchy",
      discovered: false, nearby: true }]
    button = findChild(item, "fleetEnrollButton")
    verify(!button.visible, "a restored fleet member does not need re-enrollment")
  }

  function test_quickConnectionFinishesConnector_data() {
    return [{ tag: "ready first", readyFirst: true }, { tag: "attempt ends first", readyFirst: false }]
  }

  function test_quickConnectionFinishesConnector(data) {
    var item = createTemporaryObject(view, this, { connectionSpinVisible: true })
    var trace = findChild(item, "connectionTrace")
    wait(80)
    var progress = item.connectionProgress
    verify(progress > 0 && progress < 0.1)
    if (data.readyFirst) item.connectionComplete = true
    item.connectionSpinVisible = false
    if (!data.readyFirst) item.connectionComplete = true
    wait(40)
    verify(trace.visible && trace.opacity > 0.9, "trace stays visible when connection confirms")
    verify(item.connectionProgress > progress && item.connectionProgress < 1, "remaining wire animates instead of snapping")
    tryCompare(item, "connectionProgress", 1, 650)
    verify(trace.visible && trace.opacity > 0.9, "completed wire holds before fading")
    tryCompare(item, "connectionAnimating", false, 650)
    verify(!trace.visible)
    compare(item.connectionProgress, 1)
  }

  // 2026-10-12: taking the seat from the Gram drew a trace to the label, then the seat dropped to dashed while the XPS
  // glided in. Switching keeps the connector solid without replaying a connection trace.
  function test_aSeatSwitchDrawsNoTrace() {
    var item = createTemporaryObject(view, this, { attemptKind: "switching", connectionSpinVisible: true })
    wait(200)
    verify(!item.connectionAnimating, "no trace while switching")
    item.attemptKind = "none"
    wait(200)
    verify(!item.connectionAnimating, "the trace does not start partway through the switch")
    item.connectionComplete = true
    item.connectionSpinVisible = false
    wait(200)
    verify(!item.connectionAnimating, "nothing to finish")
    item.connectionComplete = false
    item.connectionSpinVisible = true
    wait(100)
    verify(item.connectionAnimating, "the next ordinary attempt draws its trace")
  }

  // 2026-10-14: a switch that starts while the trace is already drawing
  // (Gram activity lands mid-reconnect) must not interrupt it: the trace
  // finishes to the label and the connected state takes over, instead of
  // the seat flashing its disconnected state in between.
  function test_aSwitchStartingMidTraceLetsItFinish() {
    var item = createTemporaryObject(view, this, { connectionSpinVisible: true })
    wait(80)
    verify(item.connectionAnimating, "trace is drawing")
    var progress = item.connectionProgress
    verify(progress > 0, "trace has started along the wire")
    item.attemptKind = "switching"
    wait(150)
    verify(item.connectionAnimating, "the switch does not interrupt the trace")
    verify(item.connectionProgress > progress, "the trace keeps drawing")
    item.connectionComplete = true
    item.connectionSpinVisible = false
    item.attemptKind = "none"
    tryCompare(item, "connectionProgress", 1, 650)
    tryCompare(item, "connectionAnimating", false, 900)
  }

  // 2026-10-14: while the connection trace animates toward the Omarchy
  // seat (this laptop connecting while two Apple devices hold the pods),
  // the seat must not flash its disconnected look: no dashed join, and
  // "Connecting…" shows inside it at the dim label treatment.
  function test_connectingTraceKeepsOmarchySeatOutOfDisconnectedState() {
    var item = createTemporaryObject(view, this, {
      connectionSpinVisible: true, attemptKind: "connecting", omarchyTransitionLabel: "Connecting…"
    })
    wait(150)
    verify(item.connectionAnimating, "trace is drawing")
    var seat = findChild(item, "omarchySeat")
    verify(seat.connectorConnected, "seat reads as connecting while the trace runs")
    verify(!findChild(item, "seatJoinDashedOmarchy").visible, "no dashed join during the trace")
    verify(findChild(item, "seatJoinOmarchy").visible, "solid join during the trace")
    // "Connecting…" names the attempt inside the seat at the dim label
    // treatment (seat colour at ellipse opacity), like "Waiting…".
    var status = findChild(item, "seatStatusOmarchy")
    tryCompare(status, "opacity", seat.ellipseOpacity, 1000, "Connecting… shows dim while the line animates")
    compare(status.text, "Connecting…")
    // The spin ends but the attempt is still in flight (the label still
    // reports it): the trace keeps sweeping and the seat stays out of
    // its disconnected state, rather than standing still like a switch.
    item.connectionSpinVisible = false
    wait(100)
    verify(item.connectionAnimating, "trace keeps animating while the attempt runs")
    verify(seat.connectorConnected, "seat stays connecting while the attempt runs")
    verify(!findChild(item, "seatJoinDashedOmarchy").visible, "still no dashed join")
    fuzzyCompare(findChild(item, "seatStatusOmarchy").opacity, seat.ellipseOpacity, 0.01, "label stays dim")
    // The attempt ends without connecting: the trace stops and the
    // disconnected look returns.
    item.attemptKind = "none"
    item.omarchyTransitionLabel = ""
    tryCompare(item, "connectionAnimating", false, 1000)
    verify(!seat.connectorConnected, "attempt over")
    verify(findChild(item, "seatJoinDashedOmarchy").visible, "disconnected look returns after a failed attempt")
  }

  // 2026-10-14: clicking Reconnect on the Apple seat makes the bar ring
  // spin right away, but the Omarchy seat is only "Waiting…" for the
  // Apple device: the trace must not play (or loop) there. It starts
  // when waiting ends and this laptop actually attempts, and stops if
  // waiting resumes mid-draw.
  function test_traceWaitsUntilTheAttemptStarts() {
    var item = createTemporaryObject(view, this, {
      connectionSpinVisible: true, attemptKind: "waiting", omarchyTransitionLabel: "Waiting…"
    })
    wait(250)
    verify(!item.connectionAnimating, "no trace while waiting for the Apple device")
    item.attemptKind = "connecting"
    item.omarchyTransitionLabel = "Connecting…"
    wait(150)
    verify(item.connectionAnimating, "trace starts when the attempt does")
    item.attemptKind = "waiting"
    item.omarchyTransitionLabel = "Waiting…"
    tryCompare(item, "connectionAnimating", false, 1000)
  }

  // Candidate 6: the panel forwards one presentation; every seat fact
  // binds from it, and a new presentation moves them together.
  function test_presentationDrivesTheSeatAsOneInput() {
    var phone = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple", onAcl: true }
    var item = createTemporaryObject(view, this, {
      presentation: {
        seats: { apple: [phone], omarchy: [] }, availableApple: [], availableOmarchy: [],
        linuxHeld: false, viewOwnershipUnknown: false, connectedToOmarchy: false,
        audioPlaying: false, omarchyTransitionLabel: "", appleTrayAvailable: false,
        appleSeatUnknown: false, appleTrayRemembered: false,
        appleOwnsAudio: true, omarchyOwnsAudio: false, audioDeviceAddress: phone.address,
        attempt: { kind: "none", label: "" }
      }
    })
    wait(200)
    compare(findChild(item, "appleSeat").occupantLabel, "iPhone")
    verify(findChild(item, "appleSeat").ownsAudio)
    item.presentation = {
      seats: { apple: [], omarchy: [] }, availableApple: [], availableOmarchy: [],
      linuxHeld: false, viewOwnershipUnknown: false, connectedToOmarchy: false,
      audioPlaying: false, omarchyTransitionLabel: "Waiting…", appleTrayAvailable: false,
      appleSeatUnknown: false, appleTrayRemembered: false,
      appleOwnsAudio: false, omarchyOwnsAudio: false, audioDeviceAddress: "",
      attempt: { kind: "waiting", label: "Waiting…" }
    }
    wait(100)
    compare(item.attemptKind, "waiting")
    compare(item.omarchyTransitionLabel, "Waiting…")
    verify(!findChild(item, "appleSeat").ownsAudio)
  }

  function test_handoffKeepsConnectorSolidUntilArrivalOrFailure() {
    var gram = { address: "02:00:00:00:00:01", label: "Gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "XPS", onAcl: true }
    var item = createTemporaryObject(view, this, {
      omarchyDevices: [gram], availableOmarchy: [xps], connectedToOmarchy: true
    })
    var wire = findChild(item, "omarchyLink")
    var dashedJoin = findChild(item, "seatJoinDashedOmarchy")
    verify(wire.solid)
    item.attemptKind = "switching"
    item.omarchyDevices = []
    item.availableOmarchy = [gram, xps]
    item.connectedToOmarchy = false
    wait(100)
    verify(wire.solid, "waiting for release keeps the wire solid")
    verify(!dashedJoin.visible, "the connector above the label stays solid too")
    item.omarchyDevices = [xps]
    item.availableOmarchy = [gram]
    item.connectedToOmarchy = true
    wait(50)
    item.attemptKind = "none"
    for (var elapsed = 0; elapsed < 600; elapsed += 50) {
      wait(50)
      verify(wire.solid, "arrival does not interrupt the connector")
    }
    item.attemptKind = "switching"
    item.omarchyDevices = []
    item.availableOmarchy = [gram, xps]
    item.connectedToOmarchy = false
    wait(50)
    verify(wire.solid)
    item.attemptKind = "none"
    wait(50)
    verify(!wire.solid, "a failed or abandoned switch is disconnected")
    verify(dashedJoin.visible)
  }

  function test_connectorFailureAndRetry() {
    var item = createTemporaryObject(view, this, { connectionSpinVisible: true })
    wait(60)
    item.connectionSpinVisible = false
    tryCompare(item, "connectionAnimating", false)
    compare(item.connectionProgress, 0)
    item.connectionSpinVisible = true
    wait(60)
    item.connectionComplete = true
    item.connectionSpinVisible = false
    wait(80)
    compare(item.connectorPhase, 2)
    item.connectionComplete = false
    item.connectionSpinVisible = true
    wait(60)
    compare(item.connectorPhase, 1)
    verify(item.connectionProgress < 0.1, "new attempt starts a new trace")
    wait(850)
    compare(item.connectorPhase, 1, "old finish cannot stop the new trace")
  }

  function test_retainedSourceDoesNotHideAppleOwner() {
    var local = "02:00:00:00:00:01"
    var phone = "02:00:00:00:00:02"
    var previousPhone = "02:00:00:00:00:03"
    var devices = {}
    devices[phone] = { label: "iPhone", kind: "apple" }
    devices[previousPhone] = { label: "Old phone", kind: "apple" }
    var seats = Model.seatOccupants([phone], local, "Laptop", devices, true, previousPhone)
    var item = createTemporaryObject(view, this, {
      appleDevices: seats.apple, omarchyDevices: seats.omarchy,
      appleOwnsAudio: true, activeDeviceAddress: previousPhone
    })
    var icon = findChild(item, "deviceApple0")
    verify(icon.selected)
    verify(!findChild(item, "deviceOmarchy0").selected)

    item.appleOwnsAudio = false
    verify(!icon.selected)
    item.appleOwnsAudio = true
    item.appleDevices = [Object.assign({}, seats.apple[0], { onAcl: false })]
    verify(!icon.selected)

    item.appleDevices = [seats.apple[0], { address: previousPhone, label: "Old phone", onAcl: true }]
    verify(!icon.selected)
    verify(findChild(item, "deviceApple1").selected)
    item.activeDeviceAddress = "02:00:00:00:00:04"
    verify(!icon.selected)
    verify(!findChild(item, "deviceApple1").selected)
  }

  SignalSpy { id: orderDrawn; signalName: "orderDrawn" }

  function test_restartKeepsTheSavedPairOrder() {
    var first = { address: "02:00:00:00:00:01", label: "Laptop A", onAcl: true }
    var second = { address: "02:00:00:00:00:02", label: "Laptop B", onAcl: true }
    var keys = [Model.normalizeAddress(first.address), Model.normalizeAddress(second.address)]
    // Both devices are already seated at startup, so no arrival orders them.
    var fresh = createTemporaryObject(view, this, { omarchyDevices: [first, second] })
    compare(findChild(fresh, "omarchySeat").occupant.address, first.address)

    var item = createTemporaryObject(view, this, {
      savedOrders: { omarchy: { pair: [keys[1], keys[0]] } }, omarchyDevices: [first, second]
    })
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupant.address, second.address)
    compare(seat.occupantTwo.address, first.address)
  }

  function test_aLateSavedOrderReplacesOnlyAGuessedPairOrder() {
    var first = { address: "02:00:00:00:00:01", label: "Laptop A", onAcl: true }
    var second = { address: "02:00:00:00:00:02", label: "Laptop B", onAcl: true }
    var keys = [Model.normalizeAddress(first.address), Model.normalizeAddress(second.address)]
    var item = createTemporaryObject(view, this, { omarchyDevices: [first, second] })
    var seat = findChild(item, "omarchySeat")
    item.savedOrders = { omarchy: { pair: [keys[1], keys[0]] } }
    compare(seat.occupant.address, second.address)

    // An arrival decides the order; a saved order cannot override it.
    var arrived = createTemporaryObject(view, this, { omarchyDevices: [first] })
    seat = findChild(arrived, "omarchySeat")
    orderDrawn.target = arrived; orderDrawn.clear()
    arrived.omarchyDevices = [first, second]
    compare(seat.occupant.address, second.address, "the newcomer takes the left slot")
    compare(orderDrawn.count, 1)
    compare(orderDrawn.signalArguments[0][0], "omarchy")
    compare(orderDrawn.signalArguments[0][1], "pair")
    compare(orderDrawn.signalArguments[0][2], [keys[1], keys[0]])
    arrived.savedOrders = { omarchy: { pair: [keys[0], keys[1]] } }
    compare(seat.occupant.address, second.address)
  }

  function test_restartKeepsARisenPairOnItsSeatSides() {
    var first = { address: "02:00:00:00:00:01", label: "Laptop A" }
    var second = { address: "02:00:00:00:00:02", label: "Laptop B" }
    var keys = [Model.normalizeAddress(first.address), Model.normalizeAddress(second.address)]
    var item = createTemporaryObject(view, this, {
      savedOrders: { omarchy: { pair: [keys[1], keys[0]], tray: [keys[0], keys[1]] } },
      availableOmarchy: [first, second]
    })
    // The last seated pair outranks an older tray order, as it does before a restart.
    compare(findChild(item, "waitingOmarchy0").device.address, second.address)
    compare(findChild(item, "waitingOmarchy1").device.address, first.address)
  }

  function test_haloFollowsDeviceWithinSharedSeat() {
    var first = { address: "02:00:00:00:00:01", label: "Laptop A", onAcl: true }
    var second = { address: "02:00:00:00:00:02", label: "Laptop B", onAcl: true }
    var phone = { address: "02:00:00:00:00:03", label: "iPhone", onAcl: true }
    var item = createTemporaryObject(view, this, {
      omarchyDevices: [first, second], appleDevices: [phone],
      omarchyOwnsAudio: true, activeDeviceAddress: second.address
    })
    verify(item)
    var a = findChild(item, "deviceOmarchy0")
    var b = findChild(item, "deviceOmarchy1")
    var p = findChild(item, "deviceApple0")
    verify(!a.selected)
    verify(b.selected)
    // Three brightness tiers: the non-owner seated devices draw at the
    // middle (active) level, the owner at full; the glow stays owner-only.
    compare(a.level, 1)
    compare(b.level, 2)
    compare(p.level, 1)
    verify(!p.selected)
    var aName = findChild(item, "nameOmarchy0")
    var bName = findChild(item, "nameOmarchy1")
    compare(aName.opacity, 0.75)
    compare(bName.opacity, 1)
    var aOutline = findChild(a, "outline")
    compare(aOutline.opacity, 1)
    verify(Math.abs(aOutline.color.r * 255 - 0x9f) < 1.5
      && Math.abs(aOutline.color.g * 255 - 0x9f) < 1.5
      && Math.abs(aOutline.color.b * 255 - 0xa0) < 1.5, "mid tier tint, got " + aOutline.color)
    verify(Qt.colorEqual(findChild(b, "outline").color, "#e6e6e6"))
    item.activeDeviceAddress = first.address
    verify(a.selected)
    verify(!b.selected)
    item.activeDeviceAddress = ""
    verify(!a.selected)
    verify(!b.selected)
    item.appleOwnsAudio = true
    item.omarchyOwnsAudio = false
    item.activeDeviceAddress = phone.address
    verify(p.selected)
    verify(!a.selected)
    verify(!b.selected)
    item.appleDevices = [Object.assign({}, phone, { onAcl: false })]
    verify(!p.selected)
  }

  function test_localLaptopMovesFromWaitingToConnectedSeat() {
    var address = "02:00:00:00:00:01"
    var devices = { "02:00:00:00:00:01": { label: "laptop-a", kind: "omarchy", nearby: true } }
    var item = createTemporaryObject(view, this, {
      omarchyDevices: Model.seatOccupants([], address, "laptop-a", devices, false).omarchy,
      availableOmarchy: Model.availableDevices("omarchy", [], devices, address, "", false),
      connectedToOmarchy: false
    })
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupant, null)
    compare(seat.waiterLabel, "laptop-a")
    compare(seat.linkState, "disconnected")
    item.omarchyDevices = Model.seatOccupants([], address, "laptop-a", devices, true).omarchy
    item.availableOmarchy = Model.availableDevices("omarchy", [], devices, address, "", true)
    item.connectedToOmarchy = true
    compare(seat.occupantLabel, "laptop-a")
    compare(seat.waiterLabel, "")
    // The seat is drawn connected once the laptop has glided in from the tray.
    tryCompare(seat, "linkState", "connected", 2000)
    verify(seat.occupantOnAcl)
  }

  function test_displacedXpsLeavesSeatUntilControlIsReady() {
    var xps = "02:00:00:00:00:01", mac = "02:00:00:00:00:02"
    var status = Object.assign(Model.defaultStatus(), {
      daemonReachable: true, connected: true, bluezConnected: true,
      localAddress: xps, localName: "XPS", connectedPeers: [mac, xps],
      audioOwner: { holder: "remote", playing: true }, remoteSourceAddress: mac,
      controlConnection: { stage: "ready" }
    })
    var shown = Model.seatPresentation(status, false, false)
    var item = createTemporaryObject(view, this, {
      omarchyDevices: shown.seats.omarchy, availableOmarchy: shown.availableOmarchy,
      connectedToOmarchy: shown.connectedToOmarchy, omarchyTransitionLabel: shown.omarchyTransitionLabel
    })
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupantLabel, "XPS")
    for (var stage of ["interrupted", "retrying", "handshake", "ready"]) {
      status.connected = stage === "handshake" || stage === "ready"
      status.controlConnection = { stage: stage, recovering: stage === "retrying" }
      shown = Model.seatPresentation(status, false, false)
      item.omarchyDevices = shown.seats.omarchy
      item.availableOmarchy = shown.availableOmarchy
      item.connectedToOmarchy = shown.connectedToOmarchy
      item.omarchyTransitionLabel = shown.omarchyTransitionLabel
      if (stage !== "ready") {
        compare(seat.occupant, null)
        compare(seat.waiterLabel, "XPS")
        compare(seat.linkState, "disconnected")
      } else {
        compare(seat.occupantLabel, "XPS")
        compare(seat.waiterLabel, "")
        tryCompare(seat, "linkState", "connected", 2000)
      }
    }
  }

  function test_lostObserverMovesPhoneToAnUnconfirmedTray() {
    var xps = "02:00:00:00:00:01", phone = "02:00:00:00:00:02", mac = "02:00:00:00:00:03"
    var names = {}
    names[phone] = { kind: "apple", label: "iPhone" }
    names[mac] = { kind: "apple", label: "Mac" }
    var ready = Object.assign(Model.defaultStatus(), { ok: true, connected: true,
      bluezConnected: true, localAddress: xps, localName: "XPS", devices: names,
      connectedPeers: [xps, phone], peerListUpdated: 1000,
      audioOwner: { holder: "remote", playing: true }, remoteSourceAddress: phone,
      left: { inEar: true }, controlConnection: { stage: "ready" } })
    var state = Model.statusLog(null, { ev: "status", t: 1000, status: ready })
    var item = createTemporaryObject(view, this)
    function apply(shown) {
      item.appleDevices = shown.seats.apple
      item.availableApple = shown.availableApple
      item.appleSeatUnknown = shown.appleSeatUnknown
      item.appleTrayAvailable = shown.appleTrayAvailable
      item.appleOwnsAudio = shown.appleOwnsAudio
      item.connectedToOmarchy = shown.connectedToOmarchy
    }
    apply(state.seat)
    var seat = findChild(item, "appleSeat")
    compare(seat.occupantLabel, "iPhone")
    var lost = Object.assign({}, ready, { connected: false, bluezConnected: false,
      connectedPeers: [], peerListUpdated: 0, remoteSourceAddress: "",
      audioOwner: { holder: "", playing: false },
      controlConnection: { stage: "interrupted", reason: "link_lost", recovering: true } })
    state = Model.statusLog(state, { ev: "status", t: 2000, status: lost })
    apply(state.seat)
    compare(seat.occupant, null)
    compare(seat.statusText, "")
    compare(seat.linkState, "disconnected")
    verify(seat.waiting.some(function (device) { return device.label === "iPhone" }))
    verify(seat.waitingUnconfirmed)
    verify(!seat.actionEnabled)
    verify(!seat.ownsAudio)
    reconnect.target = item
    release.target = item
    reconnect.clear()
    release.clear()
    item.activateApple()
    compare(reconnect.count, 0)
    compare(release.count, 0)
    var returned = Object.assign({}, ready, { connectedPeers: [xps, mac],
      peerListUpdated: 3000, remoteSourceAddress: mac })
    state = Model.statusLog(state, { ev: "status", t: 3000, status: returned })
    apply(state.seat)
    compare(seat.occupantLabel, "Mac")
    compare(seat.statusText, "")
    verify(!seat.waitingUnconfirmed)
    compare(seat.waiterLabel, "iPhone")
    tryCompare(seat, "linkState", "claimed", 2000)
  }

  function test_closedCaseShowsRememberedAppleDevices() {
    var phone = "02:00:00:00:00:02", mac = "02:00:00:00:00:03"
    var names = {}
    names[phone] = { kind: "apple", label: "iPhone" }
    names[mac] = { kind: "apple", label: "Mac" }
    var status = Object.assign(Model.defaultStatus(), { ok: true, devices: names,
      caseConfirmedDocked: true, lidState: Model.LID_CLOSED,
      left: { inCase: true, inEar: false }, right: { inCase: true, inEar: false },
      controlConnection: { stage: "waiting" } })
    var shown = Model.statusLog(null, { ev: "status", t: 1000, status: status }).seat
    var item = createTemporaryObject(view, this, {
      appleDevices: shown.seats.apple, availableApple: shown.availableApple,
      appleSeatUnknown: shown.appleSeatUnknown, appleTrayAvailable: shown.appleTrayAvailable,
      appleTrayRemembered: shown.appleTrayRemembered
    })
    var seat = findChild(item, "appleSeat")
    compare(seat.occupant, null)
    compare(seat.statusText, "")
    compare(seat.waiting.map(function (device) { return device.label }).sort(), ["Mac", "iPhone"])
    verify(seat.showWaiting)
    verify(seat.waitingUnconfirmed)
    verify(!seat.actionEnabled)
    reconnect.target = item
    reconnect.clear()
    item.activateApple()
    compare(reconnect.count, 0)
  }

  function test_disconnectedFleetObserverShowsActualSeats() {
    var status = { connected: false, localAddress: "02:00:00:00:00:03", localName: "laptop-c",
      devices: { "02:00:00:00:00:01": { label: "laptop-a", kind: "omarchy" },
        "02:00:00:00:00:02": { label: "Phone", kind: "apple" } },
      fleet: { roster: { reporter: "02:00:00:00:00:01",
        peers: ["02:00:00:00:00:01", "02:00:00:00:00:02"], audio_source: "02:00:00:00:00:02" } } }
    var shown = Model.fleetSeatView(status)
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: false, omarchyDevices: shown.seats.omarchy, appleDevices: shown.seats.apple,
      availableOmarchy: shown.availableOmarchy, appleOwnsAudio: shown.appleOwnsAudio,
      omarchyOwnsAudio: shown.omarchyOwnsAudio, activeDeviceAddress: shown.sourceAddress
    })
    var omarchy = findChild(item, "omarchySeat"), apple = findChild(item, "appleSeat")
    compare(omarchy.occupantLabel, "laptop-a")
    compare(omarchy.waiterLabel, "laptop-c")
    verify(omarchy.hasWaitingDevice)
    compare(omarchy.linkState, "connected")
    compare(apple.occupantLabel, "Phone")
    compare(apple.linkState, "claimed")
    compare(item.activeDeviceAddress, "02:00:00:00:00:02")
    verify(findChild(item, "deviceApple0").selected)
    verify(!findChild(item, "deviceOmarchy0").selected)
    verify(!item.connectedToOmarchy)
  }

  function test_connectedLaptopUsesFleetAppleSeatUntilDirectRoster() {
    var status = Object.assign(Model.defaultStatus(), {
      connected: true, bluezConnected: true, localAddress: "02:00:00:00:00:03", localName: "laptop-c",
      devices: { "02:00:00:00:00:01": { label: "laptop-a", kind: "omarchy" },
        "02:00:00:00:00:02": { label: "Phone", kind: "apple" } },
      fleet: { roster: { reporter: "02:00:00:00:00:01",
        peers: ["02:00:00:00:00:01", "02:00:00:00:00:02"], audio_source: "02:00:00:00:00:02" } }
    })
    var presentation = Model.connectionPresentation(status, null, 1000)
    verify(presentation.snapshot !== null)
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: presentation.snapshot.seats.omarchy,
      appleDevices: presentation.snapshot.seats.apple, ownershipUnknown: true
    })
    var apple = findChild(item, "appleSeat"), omarchy = findChild(item, "omarchySeat")
    compare(apple.occupantLabel, "Phone")
    compare(apple.linkState, "connected")
    compare(apple.actionLabel, "")
    // This laptop waits above the seat until its own roster confirms it.
    compare(omarchy.occupant, null)
    verify(presentation.snapshot.localWaiting)
    status.peerListUpdated = 2000
    var confirmed = Model.connectionPresentation(status, presentation, 2000)
    compare(confirmed.snapshot, null)
    item.appleDevices = Model.seatOccupants([], status.localAddress, status.localName, status.devices, true, "").apple
    compare(apple.occupant, null)
  }

  function test_remoteLaptopHighlightsOmarchyConnector() {
    var status = { connected: true, audioOwner: { holder: "remote", playing: true }, remoteSourceAddress: "02:00:00:00:00:02",
      devices: { "02:00:00:00:00:02": { label: "laptop-b", kind: "omarchy" } } }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true,
      omarchyDevices: [{ label: "laptop-a", onAcl: true }, { label: "laptop-b", onAcl: true }],
      omarchyOwnsAudio: Model.remoteOmarchyAudio(status),
      appleOwnsAudio: !Model.remoteOmarchyAudio(status)
    })
    var seat = findChild(item, "omarchySeat")
    compare(seat.linkState, "claimed")
    compare(seat.linkOpacity, 1)
    compare(seat.linkWidth, 1.25)
    compare(seat.ellipseOpacity, 0.3)
    compare(seat.ellipseWidth, 1.25)
    compare(findChild(item, "appleSeat").ownsAudio, false)
    status.devices["02:00:00:00:00:02"].kind = "apple"
    item.omarchyOwnsAudio = Model.remoteOmarchyAudio(status)
    compare(seat.linkState, "connected")
    compare(seat.linkOpacity, 0.5)
    compare(seat.ellipseOpacity, 0.3)
    compare(seat.ellipseWidth, 1.25)
  }

  function test_yieldedDeviceGlidesUpAndWaitingDeviceStaysLeft() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [gram]
    })
    wait(700)
    var before = findChild(item, "waitingOmarchy0").x
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [gram, { address: xps.address, label: xps.label }]
    wait(60)
    var rising = findChild(item, "waitingOmarchy1")
    var waiting = findChild(item, "waitingOmarchy0")
    verify(rising.glideY > 5, "rising from the seat " + rising.glideY)
    compare(waiting.glideX, 0)
    compare(waiting.x, before)
    tryCompare(rising, "glideY", 0, 1500)
    compare(waiting.glideX, 0)
    compare(waiting.x, before)
  }

  function test_trayRiseDoesNotReplayWhenNewOwnerArrives() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [gram], availableOmarchy: [xps]
    })
    wait(700)
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [xps, { address: gram.address, label: gram.label }]
    wait(60)
    var rising = findChild(item, "waitingOmarchy1")
    verify(rising.glideY > 5)
    tryCompare(rising, "glideY", 0, 1500)
    item.omarchyDevices = [{ address: xps.address, label: xps.label, onAcl: true }]
    item.availableOmarchy = [{ address: gram.address, label: gram.label }]
    wait(60)
    compare(findChild(item, "waitingOmarchy0").glideY, 0)
  }

  function test_releasedHelperStaysInTrayThroughControlSetup() {
    var local = "02:00:00:00:00:02", gram = "02:00:00:00:00:01", phone = "02:00:00:00:00:03"
    var names = {}
    names[gram] = { label: "lg-gram", kind: "omarchy" }
    names[phone] = { label: "iPhone", kind: "apple" }
    var status = Object.assign(Model.defaultStatus(), {
      ok: true, localAddress: local, localName: "omarchy-xps", devices: names,
      peerListUpdated: 1000, controlConnection: { stage: "idle" },
      fleet: { roster: { reporter: gram, peers: [phone, gram], audio_source: phone, pending: false } }
    })
    var log = null
    var item = createTemporaryObject(view, this)
    function apply(at) {
      log = Model.statusLog(log, { t: at, ev: "status", status: status })
      item.appleDevices = log.seat.seats.apple
      item.omarchyDevices = log.seat.seats.omarchy
      item.availableApple = log.seat.availableApple
      item.availableOmarchy = log.seat.availableOmarchy
      item.connectedToOmarchy = log.seat.connectedToOmarchy
      item.omarchyTransitionLabel = log.seat.omarchyTransitionLabel
    }
    apply(1000)
    wait(700)
    compare(item.omarchyDevices[0].address, gram)
    status = Object.assign({}, status, { fleet: Object.assign({}, status.fleet, {
      routing: { phase: "waiting_admission", busy: true, peer: gram, retain: "" }
    }) })
    apply(2000)
    wait(700)
    var waiting = findChild(item, "waitingOmarchy1")
    verify(waiting)
    compare(waiting.label, "lg-gram")
    var trayY = waiting.mapToItem(item, 0, 0).y
    for (var stage of ["connecting", "handshake", "notifications", "ready"]) {
      var ready = stage === "ready"
      status = Object.assign({}, status, {
        connected: stage !== "connecting", bluezConnected: true,
        controlConnection: { stage: stage },
        // Until the pods send their own list, admission has removed the
        // released helper from the daemon's cached connected_peers fallback.
        connectedPeers: ready ? [phone, local] : [phone],
        fleet: Object.assign({}, status.fleet, {
          routing: { phase: ready ? "waiting_completion" : "connecting", busy: true, peer: gram, retain: phone }
        })
      })
      apply(3000 + ["connecting", "handshake", "notifications", "ready"].indexOf(stage) * 1000)
      wait(60)
      var gramTray = findChild(item, ready ? "waitingOmarchy0" : "waitingOmarchy1")
      compare(gramTray, waiting, "Gram keeps its tray delegate during " + stage)
      compare(gramTray.glideY, 0, "Gram must not rise again during " + stage)
      fuzzyCompare(gramTray.mapToItem(item, 0, 0).y, trayY, 1)
      verify(!item.omarchyDevices.some(function (d) { return d.address === gram }))
    }
    var arriving = findChild(item, "deviceOmarchy0").parent.parent
    verify(arriving.glideY < -5, "XPS still descends from its tray")
    tryCompare(arriving, "glideY", 0, 1500)
    tryCompare(waiting, "glideX", 0, 1500)
  }

  function test_retainedIPhoneStaysSeatedWhileXpsYieldsToGram() {
    var local = "02:00:00:00:00:02", gram = "02:00:00:00:00:01", phone = "02:00:00:00:00:03"
    var names = {}
    names[gram] = { label: "lg-gram", kind: "omarchy" }
    names[phone] = { label: "iPhone", kind: "apple" }
    var status = Object.assign(Model.defaultStatus(), {
      ok: true, localAddress: local, localName: "omarchy-xps", devices: names,
      connected: true, bluezConnected: true, connectedPeers: [phone, local], peerListUpdated: 1000,
      controlConnection: { stage: "ready" }, audioOwner: { holder: "remote", playing: true },
      remoteSourceAddress: phone, rosterAudioOwner: phone
    })
    var log = null
    var item = createTemporaryObject(view, this)
    function apply(at) {
      log = Model.statusLog(log, { t: at, ev: "status", status: status })
      item.appleDevices = log.seat.seats.apple
      item.omarchyDevices = log.seat.seats.omarchy
      item.availableApple = log.seat.availableApple
      item.availableOmarchy = log.seat.availableOmarchy
      item.connectedToOmarchy = log.seat.connectedToOmarchy
      item.appleOwnsAudio = log.seat.appleOwnsAudio
      item.activeDeviceAddress = log.seat.audioDeviceAddress
    }
    apply(1000)
    wait(700)
    var icon = findChild(item, "deviceApple0")
    var position = icon.mapToItem(item, 0, 0)
    status = Object.assign({}, status, {
      connected: false, connectedPeers: [], remoteSourceAddress: "",
      controlConnection: { stage: "interrupted", recovering: true },
      fleet: { routing: { phase: "waiting_connection", busy: true, peer: gram, retain: phone } }
    })
    for (var step = 0; step < 3; step++) {
      status = Object.assign({}, status, { bluezConnected: step === 0 })
      if (step === 2)
        status.fleet = Object.assign({}, status.fleet, {
          roster: { reporter: gram, peers: [phone, gram], audio_source: phone, pending: false }
        })
      apply(2000 + step * 1000)
      wait(60)
      compare(findChild(item, "deviceApple0"), icon, "iPhone keeps its seat delegate")
      verify(icon.selected, "iPhone remains the audio owner")
      fuzzyCompare(icon.mapToItem(item, 0, 0).x, position.x, 1)
      fuzzyCompare(icon.mapToItem(item, 0, 0).y, position.y, 1)
      verify(!item.availableApple.some(function (d) { return d.address === phone }))
    }
    compare(item.omarchyDevices[0].address, gram)
    verify(item.availableOmarchy.some(function (d) { return d.address === local }))
  }

  function test_retainedIPhoneStaysSeatedWhileXpsJoinsFromGram() {
    var xps = "02:00:00:00:00:02", gram = "02:00:00:00:00:01", phone = "02:00:00:00:00:03"
    var names = {}
    names[gram] = { label: "lg-gram", kind: "omarchy" }
    names[phone] = { label: "iPhone", kind: "apple" }
    var status = Object.assign(Model.defaultStatus(), {
      ok: true, localAddress: xps, localName: "omarchy-xps", devices: names,
      fleet: { roster: { reporter: gram, peers: [phone, gram], audio_source: phone, pending: false } }
    })
    var log = null
    var item = createTemporaryObject(view, this)
    function apply(at) {
      log = Model.statusLog(log, { t: at, ev: "status", status: status })
      item.appleDevices = log.seat.seats.apple
      item.omarchyDevices = log.seat.seats.omarchy
      item.availableApple = log.seat.availableApple
      item.availableOmarchy = log.seat.availableOmarchy
      item.connectedToOmarchy = log.seat.connectedToOmarchy
      item.appleOwnsAudio = log.seat.appleOwnsAudio
      item.activeDeviceAddress = log.seat.audioDeviceAddress
    }
    apply(1000)
    wait(700)
    var icon = findChild(item, "deviceApple0")
    var position = icon.mapToItem(item, 0, 0)
    status.fleet = { routing: { phase: "connecting", busy: true, peer: gram, retain: phone } }
    for (var step = 0; step < 4; step++) {
      status = Object.assign({}, status, {
        connected: step > 0, bluezConnected: step > 0,
        controlConnection: { stage: step === 0 ? "connecting" : step === 3 ? "ready" : "notifications" },
        connectedPeers: step >= 2 ? [phone, xps] : [], peerListUpdated: step >= 2 ? 4000 : 0
      })
      apply(2000 + step * 1000)
      wait(60)
      compare(findChild(item, "deviceApple0"), icon, "iPhone keeps its seat delegate during XPS setup")
      verify(icon.selected, "iPhone remains the audio owner")
      fuzzyCompare(icon.mapToItem(item, 0, 0).x, position.x, 1)
      fuzzyCompare(icon.mapToItem(item, 0, 0).y, position.y, 1)
      verify(!item.availableApple.some(function (d) { return d.address === phone }))
    }
    compare(item.omarchyDevices[0].address, xps)
    verify(item.availableOmarchy.some(function (d) { return d.address === gram }))
  }

  function test_seatedGramSlidesToPairSlotWhenXpsJoins() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [gram], availableOmarchy: [],
      localAddress: xps.address
    })
    wait(700)
    var soloIcon = findChild(item, "deviceOmarchy0")
    var solo = soloIcon.mapToItem(item, soloIcon.width / 2, soloIcon.height / 2)
    item.omarchyDevices = [gram, xps]
    wait(120)
    var xpsIcon = findChild(item, "deviceOmarchy0")
    var gramIcon = findChild(item, "deviceOmarchy1")
    verify(xpsIcon !== gramIcon)
    verify(xpsIcon.mapToItem(item, xpsIcon.width / 2, xpsIcon.height / 2).x < solo.x,
      "the XPS descends into the left slot")
    verify(gramIcon.mapToItem(item, gramIcon.width / 2, gramIcon.height / 2).x > solo.x + 2,
      "the Gram slides right to its pair slot")
    wait(800)
    verify(gramIcon.mapToItem(item, gramIcon.width / 2, 0).x
      > xpsIcon.mapToItem(item, xpsIcon.width / 2, 0).x, "XPS holds the left slot, Gram the right")
  }

  // The seated Gram starts sliding right as soon as the XPS begins taking
  // the seat, before the XPS has landed.
  function test_seatedGramSlidesRightAsSoonAsXpsTakesOver() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [gram], availableOmarchy: [xps],
      localAddress: xps.address
    })
    wait(700)
    var soloIcon = findChild(item, "deviceOmarchy0")
    var solo = soloIcon.mapToItem(item, soloIcon.width / 2, soloIcon.height / 2)
    item.attemptKind = "switching"
    wait(200)
    var moved = findChild(item, "deviceOmarchy0")
    verify(moved.mapToItem(item, moved.width / 2, moved.height / 2).x > solo.x + 5,
      "the Gram slides right at once, before the XPS lands")
    verify(findChild(item, "waitingOmarchy0"), "the XPS is still in the tray")
  }

  function test_joiningGramTakesLeftSeatAndXpsSlidesRight() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [gram],
      localAddress: xps.address
    })
    wait(700)
    var soloIcon = findChild(item, "deviceOmarchy0")
    var solo = soloIcon.mapToItem(item, soloIcon.width / 2, soloIcon.height / 2)
    item.omarchyDevices = [xps, Object.assign({}, gram, { onAcl: true })]
    item.availableOmarchy = []
    wait(120)
    var gramIcon = findChild(item, "deviceOmarchy0")
    var xpsIcon = findChild(item, "deviceOmarchy1")
    verify(gramIcon !== xpsIcon)
    var gramMid = gramIcon.mapToItem(item, gramIcon.width / 2, gramIcon.height / 2)
    var xpsMid = xpsIcon.mapToItem(item, xpsIcon.width / 2, xpsIcon.height / 2)
    verify(gramMid.x < solo.x, "Gram descends into the left slot")
    verify(xpsMid.x > solo.x + 2, "XPS is sliding right: solo=" + solo.x + " mid=" + xpsMid.x)
    fuzzyCompare(xpsMid.y, solo.y, 1)
    wait(800)
    var gramSettled = gramIcon.mapToItem(item, gramIcon.width / 2, gramIcon.height / 2)
    var xpsSettled = xpsIcon.mapToItem(item, xpsIcon.width / 2, xpsIcon.height / 2)
    verify(gramSettled.x < xpsSettled.x, "Gram holds the left slot, XPS the right")
    fuzzyCompare(gramSettled.y, solo.y, 1)
    fuzzyCompare(xpsSettled.y, solo.y, 1)
  }

  // 2026-10-13: on Reconnect both Omarchy occupants rise together and
  // keep their seat order in the tray: left seat to left tray, right seat
  // to right tray. The displaced-device-goes-right rule must not mirror them.
  function test_reconnectPairRisesKeepingSeatOrder() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps, gram], availableOmarchy: [],
      localAddress: xps.address
    })
    wait(800)
    item.reconnectBusy = true
    item.omarchyDevices = []
    item.availableOmarchy = [Object.assign({}, xps, { onAcl: false }), Object.assign({}, gram, { onAcl: false })]
    wait(60)
    compare(findChild(item, "waitingOmarchy0").device.label, "omarchy-xps")
    compare(findChild(item, "waitingOmarchy1").device.label, "lg-gram")
    wait(900)
    compare(findChild(item, "waitingOmarchy0").device.label, "omarchy-xps")
    compare(findChild(item, "waitingOmarchy1").device.label, "lg-gram")
    var left = findChild(item, "waitingOmarchy0"), right = findChild(item, "waitingOmarchy1")
    verify(left.mapToItem(item, 0, 0).x < right.mapToItem(item, 0, 0).x)
  }

  // 2026-10-13: after a Reconnect the pair sits in the tray with the Gram
  // on the left and the XPS on the right. The XPS then takes the seat
  // alone: it must glide from the right tray slot down to the middle of
  // the seat, and the Gram must stay in the tray throughout.
  function test_xpsReturnsAloneToSeatCenterFromRightTraySlot() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [],
      localAddress: xps.address
    })
    wait(700)
    item.omarchyDevices = [xps, gram]
    wait(800)
    item.reconnectBusy = true
    item.omarchyDevices = []
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false }),
      Object.assign({}, xps, { onAcl: false })]
    wait(900)
    compare(findChild(item, "waitingOmarchy0").device.label, "lg-gram")
    compare(findChild(item, "waitingOmarchy1").device.label, "omarchy-xps")
    item.omarchyDevices = [xps]
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false })]
    wait(60)
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupantLabel, "omarchy-xps")
    var seated = findChild(item, "deviceOmarchy0").parent.parent
    verify(seated.glideY < -5, "XPS descends from the tray, glideY=" + seated.glideY)
    verify(seated.glideX > 5, "XPS comes from the right tray slot, glideX=" + seated.glideX)
    compare(findChild(item, "waitingOmarchy0").device.label, "lg-gram")
    tryCompare(seated, "glideY", 0, 1500)
    tryCompare(seated, "glideX", 0, 1500)
    var center = seat.mapToItem(item, seat.width / 2, 0).x
    var icon = findChild(item, "deviceOmarchy0")
    fuzzyCompare(icon.mapToItem(item, icon.width / 2, 0).x, center, 2)
    compare(findChild(item, "waitingOmarchy0").device.label, "lg-gram")
  }

  // 2026-10-13: the pair rises one at a time on Reconnect, the Gram first.
  // Each laptop rises to the tray side it already sat on: the right-hand
  // occupant waits on the right even while it is alone in the tray, and
  // the left-hand one joins it on the left.
  function test_pairRisesToTheTraySidesTheySatOn() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [gram], availableOmarchy: [],
      localAddress: xps.address
    })
    wait(700)
    item.omarchyDevices = [gram, xps]
    wait(800)
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupantLabel, "omarchy-xps")
    compare(seat.occupantTwoLabel, "lg-gram")
    item.reconnectBusy = true
    item.omarchyDevices = [xps]
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false })]
    wait(900)
    var trayCenter = seat.mapToItem(item, seat.width / 2, 0).x
    var gramTray = findChild(item, "waitingOmarchy0")
    verify(gramTray.mapToItem(item, gramTray.width / 2, 0).x > trayCenter,
      "the Gram rises to the right tray slot, not the left")
    item.omarchyDevices = []
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false }),
      Object.assign({}, xps, { onAcl: false })]
    wait(900)
    compare(findChild(item, "waitingOmarchy0").device.label, "omarchy-xps")
    compare(findChild(item, "waitingOmarchy1").device.label, "lg-gram")
    var left = findChild(item, "waitingOmarchy0"), right = findChild(item, "waitingOmarchy1")
    verify(left.mapToItem(item, left.width / 2, 0).x < trayCenter)
    verify(right.mapToItem(item, right.width / 2, 0).x > trayCenter)
    item.reconnectBusy = false
  }

  // 2026-10-13: once the XPS has returned to the seat after a Reconnect,
  // the Gram is alone in the tray and slides to the left slot.
  function test_gramSlidesLeftWhenXpsReturnsAfterReconnect() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [gram], availableOmarchy: [],
      localAddress: xps.address
    })
    wait(700)
    item.omarchyDevices = [gram, xps]
    wait(800)
    item.reconnectBusy = true
    item.omarchyDevices = [xps]
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false })]
    wait(900)
    item.omarchyDevices = []
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false }),
      Object.assign({}, xps, { onAcl: false })]
    wait(900)
    item.omarchyDevices = [xps]
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false })]
    item.reconnectBusy = false
    wait(200)
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupantLabel, "omarchy-xps")
    var trayCenter = seat.mapToItem(item, seat.width / 2, 0).x
    var gramTray = findChild(item, "waitingOmarchy0")
    compare(gramTray.device.label, "lg-gram")
    tryVerify(function () {
      return gramTray.mapToItem(item, gramTray.width / 2, 0).x < trayCenter
    }, 3000)
  }

  // 2026-10-13: mid-reconnect, after both laptops have risen to the tray,
  // a remembered snapshot can name the Gram as the seat's occupant again.
  // It was asked to stay off: it must stay in the tray, not glide down and
  // back up before the XPS lands.
  function test_snapshotDoesNotReseatGramDuringReconnect() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [],
      localAddress: xps.address
    })
    wait(700)
    item.omarchyDevices = [xps, gram]
    wait(800)
    item.reconnectBusy = true
    item.omarchyDevices = []
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false }),
      Object.assign({}, xps, { onAcl: false })]
    wait(900)
    compare(findChild(item, "waitingOmarchy0").device.label, "lg-gram")
    item.omarchyDevices = [Object.assign({}, gram, { onAcl: true })]
    item.availableOmarchy = []
    wait(300)
    compare(findChild(item, "waitingOmarchy0").device.label, "lg-gram")
    fuzzyCompare(findChild(item, "waitingOmarchy0").glideY, 0, 1)
    verify(!findChild(item, "deviceOmarchy0").parent.parent.visible,
      "the snapshot does not draw the Gram in the seat")
    item.omarchyDevices = [xps]
    item.availableOmarchy = [Object.assign({}, gram, { onAcl: false })]
    wait(60)
    var seat = findChild(item, "omarchySeat")
    compare(seat.occupantLabel, "omarchy-xps")
    compare(findChild(item, "waitingOmarchy0").device.label, "lg-gram")
    item.reconnectBusy = false
  }

  function test_fleetRefreshKeepsTheTrayRiseInProgress() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [gram], availableOmarchy: [xps]
    })
    wait(700)
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [xps, { address: gram.address, label: gram.label }]
    wait(180)
    var rising = findChild(item, "waitingOmarchy1")
    var before = rising.mapToItem(item, 0, 0).y
    verify(rising.glideY > 5)
    item.availableOmarchy = [{ address: xps.address, label: xps.label },
      { address: gram.address, label: gram.label, onAcl: false }]
    wait(20)
    var refreshed = findChild(item, "waitingOmarchy1")
    compare(refreshed, rising)
    verify(Math.abs(refreshed.mapToItem(item, 0, 0).y - before) < 10, "refresh preserves the in-flight position")
    verify(refreshed.glideY > 5, "the rise is still in progress")
    var beforeMove = refreshed.mapToItem(item, 0, 0)
    item.omarchyDevices = [{ address: xps.address, label: xps.label, onAcl: true }]
    item.availableOmarchy = [{ address: gram.address, label: gram.label }]
    wait(20)
    var shifted = findChild(item, "waitingOmarchy0")
    compare(shifted, rising)
    var afterMove = shifted.mapToItem(item, 0, 0)
    verify(Math.abs(afterMove.x - beforeMove.x) < 10, "changing tray slots preserves horizontal position")
    verify(Math.abs(afterMove.y - beforeMove.y) < 10, "changing tray slots keeps the rise in progress")
    wait(300)
    compare(shifted.glideY, 0)
  }

  // 2026-10-04: icons and labels popped to their new size instead of growing or shrinking with the glide.
  function test_sizeTweensWithTheGlide() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [gram]
    })
    wait(1500)
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [gram, { address: xps.address, label: xps.label }]
    wait(60)
    var rising = findChild(item, "waitingOmarchy1")
    var risingIcon = rising.children[0].children[0]
    verify(risingIcon.scale > 0.8, "leaving the seat starts near full size " + risingIcon.scale)
    verify(rising.children[1].scale > 1.05, "label starts near the seated size " + rising.children[1].scale)
    tryVerify(function () { return Math.abs(risingIcon.scale - 0.75) < 0.01 }, 1500)
    wait(1500)
    item.connectedToOmarchy = true
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = [{ address: xps.address, label: xps.label }]
    wait(60)
    var seated = findChild(item, "deviceOmarchy0")
    verify(seated.scale < 0.95, "entering the seat starts near the waiting size " + seated.scale)
    verify(findChild(item, "nameOmarchy0").scale < 0.95, "label starts near the waiting size")
    tryVerify(function () { return Math.abs(seated.scale - 1) < 0.01 }, 1500)
  }

  // A departing device slides sideways early while its rise starts slowly.
  function test_leavingTheSeatStartsSlowly() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [gram]
    })
    wait(1500)
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [gram, { address: xps.address, label: xps.label }]
    wait(30)
    var rising = findChild(item, "waitingOmarchy1")
    var start = rising.glideY
    var startX = Math.abs(rising.glideX)
    verify(start > 5, "rising from the seat " + start)
    verify(startX > 1, "moving sideways into the right tray slot")
    wait(120)
    verify(rising.glideY > start * 0.8, "still close to the seat after 150 ms " + rising.glideY + " of " + start)
    verify(Math.abs(rising.glideX) < startX * 0.4, "horizontal motion leads the rise " + rising.glideX)
    verify(Math.abs(rising.glideX) > 0, "horizontal motion continues over the full tween")
    tryCompare(rising, "glideY", 0, 1500)
  }

  // 2026-10-04: the device that stays waits for the leaver to finish before it recenters.
  function test_trayNeighbourDoesNotLeaveSeatWhenXpsJoins_data() {
    return [{tag: "fresh tray", previousSeat: false}, {tag: "Gram previously seated", previousSeat: true}]
  }

  function test_trayNeighbourDoesNotLeaveSeatWhenXpsJoins(data) {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: data.previousSeat,
      omarchyDevices: data.previousSeat ? [Object.assign({}, gram, {onAcl: true})] : [],
      availableOmarchy: data.previousSeat ? [xps] : [xps, gram]
    })
    wait(700)
    if (data.previousSeat) {
      item.connectedToOmarchy = false
      item.omarchyDevices = []
      item.availableOmarchy = [xps, gram]
      wait(700)
    }
    var staying = findChild(item, "waitingOmarchy1")
    var before = staying.mapToItem(item, 0, 0)
    item.connectedToOmarchy = true
    item.omarchyDevices = [Object.assign({}, xps, {onAcl: true})]
    item.availableOmarchy = [gram]
    wait(40)
    var after = findChild(item, "waitingOmarchy0")
    compare(after, staying, "keep the Gram tray delegate")
    fuzzyCompare(after.mapToItem(item, 0, 0).y, before.y, 1)
    fuzzyCompare(after.mapToItem(item, 0, 0).x, before.x, 1)
    var seated = findChild(item, "deviceOmarchy0").parent.parent
    verify(seated.glideY < -5, "XPS descends from the tray first")
    wait(150)
    // A status refresh while XPS descends must not replay Gram's departure.
    // Gram has only just started into the vacated slot (staggered rotation).
    item.availableOmarchy = [Object.assign({}, gram)]
    fuzzyCompare(after.mapToItem(item, 0, 0).y, before.y, 1)
    verify(after.mapToItem(item, 0, 0).x <= before.x + 1, "Gram slides toward the vacated slot")
    verify(after.glideX > 5, "Gram is already sliding while XPS still descends, glideX=" + after.glideX)
    tryCompare(seated, "glideY", 0, 1500)
    tryCompare(after, "glideX", 0, 1500)
    fuzzyCompare(after.mapToItem(item, 0, 0).y, before.y, 1)
    verify(after.mapToItem(item, 0, 0).x < before.x - 10)
  }

  function test_trayWaitsBeforeRecentering() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var spare = { address: "02:00:00:00:00:05", label: "spare-laptop" }
    var item = createTemporaryObject(view, this, { omarchyDevices: [], availableOmarchy: [gram, spare] })
    wait(1500)
    var before = findChild(item, "waitingOmarchy1").mapToItem(item, 0, 0).x
    item.connectedToOmarchy = true
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = [spare]
    wait(40)
    var staying = findChild(item, "waitingOmarchy0")
    compare(Math.round(staying.mapToItem(item, 0, 0).x), Math.round(before))
    tryCompare(staying, "glideX", 0, 2000)
    verify(Math.abs(staying.mapToItem(item, 0, 0).x - before) > 5, "recentered")
  }

  function seatPair() {
    return createTemporaryObject(view, this, { appleDevices: [
      { address: "02:00:00:00:00:02", label: "iPhone", onAcl: true },
      { address: "02:00:00:00:00:04", label: "MacBook Pro", onAcl: true }] })
  }
  function iconX(item, label) {
    for (var i = 0; i < 2; i++) {
      var name = findChild(item, "nameApple" + i)
      if (name && name.parent.visible && name.text === label) return name.parent.mapToItem(item, name.parent.width / 2, 0).x
    }
    return -1
  }

  function test_seatWaitsBeforeRecenteringWhenTheSecondLeaves() {
    var item = seatPair()
    wait(1500)
    var before = iconX(item, "iPhone")
    item.appleDevices = [{ address: "02:00:00:00:00:02", label: "iPhone", onAcl: true }]
    item.availableApple = [{ address: "02:00:00:00:00:04", label: "MacBook Pro", kind: "apple" }]
    wait(250)
    compare(Math.round(iconX(item, "iPhone")), Math.round(before))
    wait(1500)
    verify(Math.abs(iconX(item, "iPhone") - before) > 5, "recentered")
  }

  function test_seatWaitsBeforeRecenteringWhenTheFirstLeaves() {
    var item = seatPair()
    wait(1500)
    var before = iconX(item, "MacBook Pro")
    item.appleDevices = [{ address: "02:00:00:00:00:04", label: "MacBook Pro", onAcl: true }]
    item.availableApple = [{ address: "02:00:00:00:00:02", label: "iPhone", kind: "apple" }]
    wait(250)
    compare(Math.round(iconX(item, "MacBook Pro")), Math.round(before))
    wait(1500)
    verify(Math.abs(iconX(item, "MacBook Pro") - before) > 5, "recentered")
  }

  function test_returningFromTheTrayGlidesFromTheTray() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [{ address: gram.address, label: gram.label, onAcl: true }] })
    wait(1500)
    item.omarchyDevices = []
    item.availableOmarchy = [gram]
    wait(2500)
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = []
    wait(60)
    var seated = findChild(item, "deviceOmarchy0").parent.parent
    verify(seated.glideY < -5, "dropping from the tray " + seated.glideY)
  }

  function test_fleetRefreshKeepsTheSeatArrivalInProgress() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, { availableOmarchy: [gram] })
    wait(700)
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = []
    wait(150)
    var icon = findChild(item, "deviceOmarchy0")
    var point = icon.mapToItem(item, icon.width / 2, icon.height)
    var scale = icon.scale
    verify(scale > 0.75 && scale < 1)
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true, connected: true }]
    wait(20)
    var refreshed = findChild(item, "deviceOmarchy0")
    var after = refreshed.mapToItem(item, refreshed.width / 2, refreshed.height)
    verify(Math.abs(after.x - point.x) < 10, "refresh preserves arrival x")
    verify(Math.abs(after.y - point.y) < 10, "refresh preserves arrival y")
    verify(refreshed.scale >= scale && refreshed.scale < 1, "refresh preserves arrival scale")
    tryCompare(refreshed, "scale", 1, 1000)
  }

  function test_seatArrivalStartsAtTheScaledTrayIcon_data() {
    return [{ tag: "left tray slot", index: 0 }, { tag: "right tray slot", index: 1 },
      { tag: "recently seated", index: 1, recentSeat: true, trayWait: 600 },
      { tag: "return during rise", index: 1, recentSeat: true, trayWait: 180 }]
  }

  function test_seatArrivalStartsAtTheScaledTrayIcon(data) {
    var devices = [
      { address: "02:00:00:00:00:01", label: "lg-gram" },
      { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    ]
    var item = createTemporaryObject(view, this, {
      omarchyDevices: data.recentSeat ? [devices[data.index]] : [],
      availableOmarchy: data.recentSeat ? [devices[1 - data.index]] : devices
    })
    wait(700)
    if (data.recentSeat) {
      item.omarchyDevices = []
      item.availableOmarchy = devices
      wait(data.trayWait)
    }
    var tray = findChild(item, "waitingOmarchy" + data.index)
    var trayIcon = tray.children[0].children[0]
    var source = trayIcon.mapToItem(item, trayIcon.width / 2, trayIcon.height)
    var sourceScale = trayIcon.scale
    var sourceLabelSize = tray.children[1].font.pixelSize * tray.children[1].scale
    var sourceLabel = tray.children[1].mapToItem(item, tray.children[1].width / 2, 0)
    item.omarchyDevices = [devices[data.index]]
    item.availableOmarchy = [devices[1 - data.index]]
    wait(1)
    var icon = findChild(item, "deviceOmarchy0")
    var label = findChild(item, "nameOmarchy0")
    var start = icon.mapToItem(item, icon.width / 2, icon.height)
    fuzzyCompare(start.x, source.x, 2)
    fuzzyCompare(start.y, source.y, 2)
    fuzzyCompare(icon.scale, sourceScale, 0.02)
    fuzzyCompare(label.font.pixelSize * label.scale, sourceLabelSize, 0.2)
    var labelStart = label.mapToItem(item, label.width / 2, 0)
    fuzzyCompare(labelStart.x, sourceLabel.x, 2)
    fuzzyCompare(labelStart.y, sourceLabel.y, 2)
    wait(150)
    verify(icon.scale > sourceScale && icon.scale < 1)
    verify(label.font.pixelSize * label.scale > sourceLabelSize && label.scale < 1)
    tryCompare(icon, "scale", 1, 1000)
    tryCompare(label, "scale", 1, 1000)
  }

  // 2026-10-04: an empty seat needs no "Empty" caption; only an action such as Reconnect is drawn there.
  function test_emptySeatHasNoCaption() {
    var item = createTemporaryObject(view, this, {})
    verify(!findChild(item, "seatActionOmarchy").visible)
    verify(!findChild(item, "seatActionApple").visible)
  }

  // 2026-10-04: on lg-gram an icon flashed at its destination for a frame before its glide began.
  function test_movingIconIsNotDrawnAtItsDestinationBeforeItsGlide() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, { omarchyDevices: [], availableOmarchy: [gram, xps] })
    wait(1500)
    item.connectedToOmarchy = true
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = [xps]
    var seated = findChild(item, "deviceOmarchy0").parent.parent
    verify(seated.opacity === 0 || seated.glideY !== 0, "drawn unshifted at the seat before the glide")
    var waiting = findChild(item, "waitingOmarchy0")
    verify(waiting.opacity === 0 || waiting.glideX !== 0, "drawn unshifted in the tray before the glide")
    wait(60)
    verify(seated.opacity === 1 && seated.glideY < -5, "gliding into the seat")
  }

  // 2026-10-04: the seat turned solid as soon as the link rose, before its device had glided in.
  function test_seatTurnsSolidWhenItsDeviceArrives() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, { omarchyDevices: [], availableOmarchy: [gram, xps] })
    var seat = findChild(item, "omarchySeat")
    wait(1500)
    compare(seat.drawnConnected, false)
    item.connectedToOmarchy = true
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = [xps]
    wait(150)
    compare(seat.drawnConnected, false)
    tryCompare(seat, "drawnConnected", true, 2000)
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [gram, xps]
    compare(seat.drawnConnected, false)
  }

  function test_connectedSeatIsSolidFromTheFirstFrame() {
    var item = createTemporaryObject(view, this, { connectedToOmarchy: true,
      omarchyDevices: [{ address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }] })
    compare(findChild(item, "omarchySeat").drawnConnected, true)
    wait(100)
    compare(findChild(item, "omarchySeat").drawnConnected, true)
  }

  function test_pairedConnectorsHaveEqualPaintedWidth_data() {
    return [
      { tag: "light", foreground: "#000000", background: "#ffffff" },
      { tag: "dark", foreground: "#a9b1d6", background: "#1a1b26" }
    ]
  }
  function test_pairedConnectorsHaveEqualPaintedWidth(data) {
    // The software backend ignores Rectangle antialiasing. Measure the same
    // GPU rasterization used by the live shell (QT_QUICK_BACKEND=rhi).
    if (GraphicsInfo.api === GraphicsInfo.Software)
      skip("Fractional stroke coverage requires the GPU renderer")
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, { connectedToOmarchy: true, omarchyOwnsAudio: true,
      foreground: data.foreground, screenFill: data.background,
      omarchyDevices: [gram, xps], activeDeviceAddress: gram.address })
    wait(700)
    var seat = findChild(item, "omarchySeat")
    var active = findChild(item, "seatJoinOmarchy"), other = findChild(item, "seatJoinSecondOmarchy")
    compare(active.width, seat.linkWidth)
    compare(other.width, seat.linkWidth)
    var shot = grabImage(seat)
    var scale = Math.round(shot.width / seat.width)
    function paintedWidth(join) {
      var y = Math.floor((join.y + join.height / 2) * scale), sum = 0
      var background = shot.red(Math.floor(join.x * scale) - 3, y)
      var contrast = Math.abs(background - item.foreground.r * 255)
      for (var x = Math.floor(join.x * scale) - 2; x <= Math.ceil((join.x + join.width) * scale) + 2; ++x)
        sum += Math.abs(shot.red(x, y) - background) / contrast
      return sum / scale
    }
    fuzzyCompare(paintedWidth(active), seat.linkWidth, 0.1)
    fuzzyCompare(paintedWidth(other), seat.linkWidth, 0.1)
  }

  // 2026-10-04: a connected seat extends its connector to the ellipse and glows where they meet.
  function test_connectedSeatJoinsAndGlowsAtTheConnector() {
    var item = createTemporaryObject(view, this)
    item.appleOwnsAudio = true
    item.appleDevices = [{ address: "02:00:00:00:00:02", label: "iPhone", onAcl: true }]
    wait(200)
    var seat = findChild(item, "appleSeat")
    var join = findChild(item, "seatJoinApple")
    var glow = findChild(item, "seatGlowApple")
    verify(join.visible && glow.visible)
    verify(!findChild(item, "seatJoinOmarchy").visible && !findChild(item, "seatGlowOmarchy").visible)
    // The lit join lands flush on the glow stroke's outer edge: reaching
    // into it overlapped the translucent strokes into a bright dot, and
    // stopping short left a visible gap.
    fuzzyCompare(join.joinTop, seat.ellipseBottom + glow.stroke / 2, 0.01)
    compare(Math.round(join.joinBottom), Math.round(seat.titleTop))
    var context = glow.getContext("2d")
    var rx = seat.ellipseRadiusX, ry = seat.padH * seat.padFlat, cx = glow.width / 2, cy = glow.height / 2
    function alpha(x, y) {
      var best = 0
      for (var dy = -3; dy <= 3; dy++) best = Math.max(best, context.getImageData(Math.round(x), Math.round(y + dy), 1, 1).data[3])
      return best
    }
    function single(x, y) { return context.getImageData(Math.round(x), Math.round(y), 1, 1).data[3] }
    verify(alpha(cx - rx * 0.2, cy + ry) > 40, "lit beside the connector " + alpha(cx - rx * 0.2, cy + ry))
    // 2026-10-04: a thin vector stroke with no bloom around it, reaching about twice as far along the ellipse.
    compare(single(cx - rx * 0.2, cy + ry + 2.5), 0)
    verify(alpha(cx - rx * 0.71, cy + ry * 0.71) > 40, "still bright 45 degrees round " + alpha(cx - rx * 0.71, cy + ry * 0.71))
    compare(alpha(cx - rx * 0.2, cy - ry), 0)
    // 2026-10-04: a dimmer connected ellipse so the glow stands out, and more glow on a seat that is only connected.
    compare(seat.ellipseOpacity, 0.3)
    // 2026-10-04: only the seat holding the audio glows; a background seat keeps flat, dimmed lines.
    // 2026-10-05: the background connection still reaches its ellipse, drawn as dim as the ellipse itself.
    item.appleOwnsAudio = false
    verify(join.visible && !glow.visible)
    // The join draws opaque in the pre-blended ellipse colour, not the
    // line colour at ellipse opacity, so strokes meeting cannot
    // composite into a brighter dot.
    compare(join.opacity, 1)
    verify(Qt.colorEqual(join.color, seat.ellipsePaint))
    // Two translucent strokes overlapping drew a bright dot, so the dim connector lands flush on the ellipse stroke's edge.
    fuzzyCompare(join.joinTop, seat.ellipseBottom + seat.ellipseWidth / 2, 0.01)
    item.appleOwnsAudio = true
    verify(join.visible && glow.visible)
    var dashed = findChild(item, "seatJoinDashedApple")
    verify(!dashed.visible)
    item.appleDevices = []
    item.availableApple = [{ address: "02:00:00:00:00:02", label: "iPhone", kind: "apple" }]
    verify(!join.visible && !glow.visible)
    // 2026-10-05: a disconnected seat still ties its label to its ellipse, dashed like both of them.
    verify(dashed.visible)
    compare(dashed.opacity, 1)
    var dashedPath = null
    for (var child = 0; child < dashed.data.length; ++child)
      if (dashed.data[child].hasOwnProperty("strokeColor")) dashedPath = dashed.data[child]
    verify(dashedPath && Qt.colorEqual(dashedPath.strokeColor, seat.ellipsePaint))
    compare(seat.ellipsePaint.a, 1)
    // The dashed join lands flush on the ellipse stroke's edge too; its
    // flat cap keeps the first dash from extending into the stroke.
    fuzzyCompare(dashed.joinTop, seat.ellipseBottom + seat.ellipseWidth / 2, 0.01)
    // A lone seat's ellipse stays closed; only a seated pair opens a
    // gap in it. The join landing flush is what prevents the dot.
    var arc = findChild(item, "seatOutlineApple")
    compare(arc.sweepAngle, 360)
    fuzzyCompare(dashed.joinBottom, seat.titleTop, 0.01)
  }

  // 2026-10-07: a pair gets two bright connectors; the glowing device's runs round the ellipse to it, the other's glow
  // spreads a little up its own side without reaching its device, and the arc between them stays dark.
  function test_pairedGlowRunsToTheGlowingDevice() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, { connectedToOmarchy: true, omarchyOwnsAudio: true,
      omarchyDevices: [gram, xps], activeDeviceAddress: xps.address })
    wait(700)
    var seat = findChild(item, "omarchySeat")
    var glow = findChild(item, "seatGlowOmarchy")
    var lit = findChild(item, "deviceOmarchy1")
    verify(lit.selected && glow.visible)
    var rx = seat.ellipseRadiusX, ry = seat.padH * seat.padFlat, cx = glow.width / 2, cy = glow.height / 2
    function alpha(x, y) {
      var context = glow.getContext("2d"), best = 0
      for (var dy = -3; dy <= 3; dy++) best = Math.max(best, context.getImageData(Math.round(x), Math.round(y + dy), 1, 1).data[3])
      return best
    }
    var right = lit.mapToItem(glow, lit.width / 2, 0).x > cx ? 1 : -1
    var active = findChild(item, "seatJoinOmarchy"), other = findChild(item, "seatJoinSecondOmarchy")
    verify(active.visible && other.visible, "two connectors")
    var activeX = active.x + active.width / 2 - seat.width / 2, otherX = other.x + other.width / 2 - seat.width / 2
    compare(activeX, right * seat.pairJoinOffset)
    compare(otherX, -right * seat.pairJoinOffset)
    compare(active.opacity, 1)
    compare(other.opacity, 1)
    var activeAt = cx + activeX, otherAt = cx + otherX
    verify(alpha(activeAt, cy + ry) > 100, "lit where the active connector meets the ellipse " + alpha(activeAt, cy + ry))
    verify(alpha(cx + right * rx * 0.3, cy + ry) > 40, "lit toward the device")
    verify(alpha(cx + right * (rx - 1), cy) > 40, "lit round the far side")
    var under = lit.mapToItem(glow, lit.width / 2, 0).x
    var top = cy - ry * Math.sqrt(1 - Math.pow((under - cx) / rx, 2))
    verify(alpha(under - right * 4, top) > 40, "lit up to the device " + alpha(under - right * 4, top))
    verify(alpha(cx, cy + ry) < 20, "dark between the connectors " + alpha(cx, cy + ry))
    // 2026-10-07: a third of the first gutter, and hard inner edges with no light bleeding into it.
    fuzzyCompare(Math.abs(activeX - otherX) - seat.linkWidth, 9, 0.01)
    var activeInner = activeAt - right * seat.linkWidth / 2, otherInner = otherAt + right * seat.linkWidth / 2
    verify(alpha(activeInner - right * 2, cy + ry) < 40, "hard edge inside the active connector " + alpha(activeInner - right * 2, cy + ry))
    verify(alpha(otherInner + right * 2, cy + ry) < 40, "hard edge inside the other connector " + alpha(otherInner + right * 2, cy + ry))
    verify(alpha(otherAt, cy + ry) > 100, "lit where the other connector meets the ellipse " + alpha(otherAt, cy + ry))
    var spreadX = otherAt - right * rx * 0.2, spreadY = cy + ry * Math.sqrt(1 - Math.pow((spreadX - cx) / rx, 2))
    verify(alpha(spreadX, spreadY) > 40, "the other glow spreads up its side " + alpha(spreadX, spreadY))
    verify(alpha(cx - right * (rx - 1), cy) < 20, "the other glow stops short of its device " + alpha(cx - right * (rx - 1), cy))
    var awayTop = cy - ry * Math.sqrt(1 - 0.0625)
    verify(alpha(cx - right * rx * 0.25, awayTop) < 8, "dark across the top")

    item.activeDeviceAddress = gram.address
    wait(50)
    verify(findChild(item, "deviceOmarchy0").selected)
    compare(active.x + active.width / 2 - seat.width / 2, -right * seat.pairJoinOffset)
    verify(alpha(cx - right * (rx - 1), cy) > 40, "the glow follows the other device")
    verify(alpha(cx + right * rx * 0.25, awayTop) < 8, "dark across the top after the flip")
  }


  // 2026-10-05: Reconnect drops both laptops; the one left for a moment recentered as if it alone stayed connected.
  function test_reconnectKeepsTheRemainingLaptopInItsPairSpot() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, { connectedToOmarchy: true, omarchyDevices: [gram, xps] })
    wait(700)
    function centerOf(name) { var icon = findChild(item, name); return icon.mapToItem(item, icon.width / 2, 0).x }
    var paired = centerOf("deviceOmarchy1")
    item.reconnectBusy = true
    item.omarchyDevices = [xps]
    // 2026-10-05: it flashed to the center for a frame, then slid back; every frame must stay put.
    for (var frame = 0; frame < 40; ++frame) {
      wait(16)
      fuzzyCompare(centerOf("deviceOmarchy1"), paired, 1)
    }
    var seat = findChild(item, "omarchySeat")
    var centered = seat.mapToItem(item, seat.width / 2, 0).x
    // A laptop rejoining after the seat emptied is alone again and centers, even mid-reconnect.
    item.omarchyDevices = []
    // 2026-10-05: the last laptop snapped to the seat's center as it left; it rises from its pair spot.
    item.availableOmarchy = [{ address: xps.address, label: xps.label }]
    wait(1)
    var rising = findChild(item, "waitingOmarchy0")
    var risingIcon = rising.children[0].children[0]
    fuzzyCompare(risingIcon.mapToItem(item, risingIcon.width / 2, 0).x, paired, 2)
    wait(700)
    item.omarchyDevices = [xps]
    wait(1500)
    fuzzyCompare(centerOf("deviceOmarchy0"), centered, 1)
    item.omarchyDevices = [gram, xps]
    wait(700)
    item.omarchyDevices = [xps]
    wait(1200)
    fuzzyCompare(centerOf("deviceOmarchy1"), paired, 1)
    item.reconnectBusy = false
    wait(1500)
    fuzzyCompare(centerOf("deviceOmarchy0"), centered, 1)
  }

  function test_departingDeviceTakesTheRightmostWaitingSpot() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true, omarchyDevices: [xps], availableOmarchy: [gram]
    })
    wait(700)
    item.connectedToOmarchy = false
    item.omarchyDevices = []
    item.availableOmarchy = [{ address: xps.address, label: xps.label }, gram]
    wait(100)
    compare(findChild(item, "waitingOmarchy1").children[1].text, "omarchy-xps")
    wait(1500)
    item.availableOmarchy = [{ address: xps.address, label: xps.label }, gram]
    wait(100)
    compare(findChild(item, "waitingOmarchy1").children[1].text, "omarchy-xps")
  }

  function test_waitingDeviceGlidesDownIntoTheSeat() {
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps" }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram" }
    var item = createTemporaryObject(view, this, { omarchyDevices: [], availableOmarchy: [gram, xps] })
    wait(700)
    item.connectedToOmarchy = true
    item.omarchyDevices = [{ address: gram.address, label: gram.label, onAcl: true }]
    item.availableOmarchy = [xps]
    wait(60)
    var seated = findChild(item, "deviceOmarchy0").parent.parent
    verify(seated.glideY < -5, "dropping into the seat " + seated.glideY)
    tryCompare(seated, "glideY", 0, 1500)
    tryCompare(seated, "glideX", 0, 1500)
  }

  function test_omarchySeatOrderIgnoresWhichMachineIsLocal() {
    function labelAt(item, index) {
      var kids = findChild(item, "deviceOmarchy" + index).parent.parent.children
      for (var i = 0; i < kids.length; ++i)
        if (kids[i].text !== undefined) return kids[i].text
      return ""
    }
    // Each machine lists itself last, so the two laptops pass opposite orders.
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true,
      omarchyDevices: [
        { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true },
        { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
      ]
    })
    compare(labelAt(item, 0), "lg-gram")
    compare(labelAt(item, 1), "omarchy-xps")
  }

  function test_pairedIconsMirrorEllipseCenter() {
    function centerX(item, seat) {
      return item.mapToItem(seat, item.width / 2, 0).x
    }
    function labelFor(icon) {
      var kids = icon.parent.parent.children
      for (var i = 0; i < kids.length; ++i)
        if (kids[i].text !== undefined) return kids[i]
      return null
    }
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true,
      omarchyDevices: [
        { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true },
        { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
      ],
      appleDevices: [
        { address: "02:00:00:00:00:03", label: "iPhone", onAcl: true },
        { address: "02:00:00:00:00:04", label: "MacBook Pro", onAcl: true }
      ]
    })
    var omarchy = findChild(item, "omarchySeat")
    var left = findChild(item, "deviceOmarchy0")
    var right = findChild(item, "deviceOmarchy1")
    var mid = omarchy.width / 2
    var leftX = centerX(left, omarchy)
    var rightX = centerX(right, omarchy)
    verify(Math.abs((mid - leftX) - (rightX - mid)) < 1)
    verify(rightX - leftX > left.width)
    verify(Math.abs(centerX(labelFor(left), omarchy) - leftX) < 1)
    verify(Math.abs(centerX(labelFor(right), omarchy) - rightX) < 1)
    var leftLabel = labelFor(left)
    var rightLabel = labelFor(right)
    var labelGap = rightLabel.mapToItem(omarchy, 0, 0).x - leftLabel.mapToItem(omarchy, leftLabel.width, 0).x
    verify(labelGap >= omarchy.labelGutter)

    var apple = findChild(item, "appleSeat")
    var phone = findChild(item, "deviceApple0")
    var mac = findChild(item, "deviceApple1")
    var appleMid = apple.width / 2
    var phoneX = centerX(phone, apple)
    var macX = centerX(mac, apple)
    verify(Math.abs((appleMid - phoneX) - (macX - appleMid)) < 1)
    verify(Math.abs(centerX(labelFor(phone), apple) - phoneX) < 1)
    verify(Math.abs(centerX(labelFor(mac), apple) - macX) < 1)
    var phoneLabel = labelFor(phone)
    var macLabel = labelFor(mac)
    var appleLabelGap = macLabel.mapToItem(apple, 0, 0).x - phoneLabel.mapToItem(apple, phoneLabel.width, 0).x
    verify(appleLabelGap >= apple.labelGutter)

    item.omarchyDevices = [{ address: "02:00:00:00:00:01", label: "omarchy-xps", onAcl: true }]
    tryCompare(omarchy, "occupantLabel", "omarchy-xps")
    var only = findChild(item, "deviceOmarchy0")
    // The remaining device recenters after the leaver's glide.
    tryVerify(function () { return Math.abs(centerX(only, omarchy) - omarchy.width / 2) < 1 }, 2500)
  }

  function test_ellipseCrossesLaptopScreenSides() {
    var item = createTemporaryObject(view, this, {
      connectedToOmarchy: true,
      omarchyDevices: [
        { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true },
        { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
      ]
    })
    var seat = findChild(item, "omarchySeat")
    var radiusY = seat.ellipseCenter - seat.ellipseTop
    for (var i = 0; i < 2; ++i) {
      var icon = findChild(item, "deviceOmarchy" + i)
      var origin = icon.mapToItem(seat, 0, 0)
      // The laptop screen spans x 2.5 to 32.5 and y 0 to 17.5, with 2.5 corners.
      for (var edge of [2.5, 32.5]) {
        var dx = (origin.x + edge - seat.width / 2) / seat.ellipseRadiusX
        var crossing = seat.ellipseCenter - radiusY * Math.sqrt(1 - dx * dx) - origin.y
        verify(crossing >= 2.5 && crossing <= 13.5, "edge " + edge + " crosses at " + crossing)
      }
    }
  }

  // 2026-10-04: a tall phone in the seat pushed the seats down, and a laptop pulled them back up.
  function test_seatsStayPutWhateverSitsOrWaits() {
    var item = createTemporaryObject(view, this, {})
    var seat = findChild(item, "appleSeat")
    function ellipseY() { return seat.mapToItem(item, 0, seat.ellipseTop).y }
    var empty = ellipseY()
    var height = item.implicitHeight
    item.appleDevices = [{ address: "02:00:00:00:00:02", label: "iPhone", onAcl: true }]
    item.availableOmarchy = [{ address: "02:00:00:00:00:01", label: "lg-gram" }]
    compare(ellipseY(), empty)
    item.appleDevices = []
    item.omarchyDevices = [{ address: "02:00:00:00:00:03", label: "omarchy-xps", onAcl: true }]
    item.availableApple = [{ address: "02:00:00:00:00:02", label: "iPhone", kind: "apple" }]
    compare(ellipseY(), empty)
    compare(item.implicitHeight, height)
    var phoneWaiting = findChild(item, "waitingApple0").mapToItem(item, 0, 0).y
    item.appleDevices = [{ address: "02:00:00:00:00:02", label: "iPhone", onAcl: true }]
    item.availableApple = [{ address: "02:00:00:00:00:04", label: "MacBook Pro", kind: "apple" }]
    compare(findChild(item, "waitingApple0").mapToItem(item, 0, 0).y, phoneWaiting)
  }

  function test_waitingDeviceSitsJustAboveSeat() {
    var item = createTemporaryObject(view, this, {
      availableApple: [{ label: "iPhone", kind: "apple" }]
    })
    var seat = findChild(item, "appleSeat")
    var waiting = findChild(item, "waitingApple0")
    var label = waiting.children[1]
    verify(label.font.pixelSize < 11)
    verify(waiting.children[0].children[0].scale < 1)
    var bottom = waiting.mapToItem(seat, 0, waiting.height).y
    // One gutter above the tallest seated icon, whether or not a device is seated.
    var gutter = seat.ellipseTop - bottom
    verify(Math.abs(gutter - seat.tallestSeatedRise - seat.waitingGap) <= 2, "gutter " + gutter)

    item.appleDevices = [{ label: "iPhone", onAcl: true }]
    item.availableApple = [{ label: "MacBook Pro", kind: "apple" }]
    var phone = findChild(item, "deviceApple0")
    waiting = findChild(item, "waitingApple0")
    bottom = waiting.mapToItem(seat, 0, waiting.height).y
    verify(waiting.mapToItem(seat, 0, 0).y >= 0)
    verify(bottom < phone.mapToItem(seat, 0, 0).y)
  }

  function test_waitingLabelMatchesIconStroke() {
    var item = createTemporaryObject(view, this, {
      availableApple: [{ label: "iPhone", kind: "apple" }]
    })
    var waiting = findChild(item, "waitingApple0")
    var outline = findChild(waiting, "outline")
    compare(String(waiting.children[1].color), String(outline.color))
  }

  function test_nearbyEnrolledLaptopWaitsWithoutInvitation() {
    var item = createTemporaryObject(view, this, {
      availableOmarchy: [{ address: "02:00:00:00:00:04", label: "laptop-b", kind: "omarchy", nearby: true }],
      connectedToOmarchy: true
    })
    var seat = findChild(item, "omarchySeat")
    verify(seat.hasWaitingDevice)
    compare(seat.waiterLabel, "laptop-b")
    verify(!findChild(item, "fleetEnrollButton").visible)
  }

  function test_buttonInvitesThatDevice() {
    var item = createTemporaryObject(view, this, {
      availableOmarchy: [{ address: "02:00:00:00:00:04", label: "Laptop B", kind: "omarchy", discovered: true }],
      connectedToOmarchy: true
    })
    enroll.target = item; enroll.clear()
    mouseClick(findChild(item, "fleetEnrollButton"))
    compare(enroll.count, 1)
    compare(enroll.signalArguments[0][0], "02:00:00:00:00:04")
    compare(enroll.signalArguments[0][1], "Laptop B")
  }

  function test_enrollmentWorksWithPodsUnavailable() {
    var status = Object.assign(Model.defaultStatus(), { ok: true, caseConfirmedDocked: true,
      caseLidState: Model.LID_CLOSED, controlConnection: { stage: "waiting" },
      devices: { "02:00:00:00:00:04": { label: "Laptop B", kind: "omarchy", nearby: true, discovered: true } } })
    var log = Model.statusLog(null, { t: 30000, ev: "status", status: status })
    var item = createTemporaryObject(view, this, {
      visible: log.showSeats, availableOmarchy: log.seat.availableOmarchy,
      connectedToOmarchy: log.seat.connectedToOmarchy, caseState: "closed"
    })
    verify(item.visible)
    verify(!item.connectedToOmarchy)
    verify(findChild(item, "appleSeat").visible)
    verify(findChild(item, "omarchySeat").visible)
    var button = findChild(item, "fleetEnrollButton")
    verify(button.visible)
    enroll.target = item; enroll.clear()
    mouseClick(button)
    compare(enroll.count, 1)
    compare(enroll.signalArguments[0][0], "02:00:00:00:00:04")
  }

  function test_bothAppleDevicesWaitAboveSeat() {
    var macDevice = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple" }
    var phoneDevice = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple", onAcl: true }
    var item = createTemporaryObject(view, this, {
      availableApple: [macDevice, phoneDevice],
      connectedToOmarchy: true, linuxHeld: true
    })
    var mac = findChild(item, "waitingApple0")
    var phone = findChild(item, "waitingApple1")
    verify(mac !== null)
    verify(phone !== null)
    compare(mac.device.label, "MacBook Pro")
    compare(phone.device.label, "iPhone")
    var seat = findChild(item, "appleSeat")
    var macPosition = mac.mapToItem(seat, 0, 0)
    var phonePosition = phone.mapToItem(seat, 0, 0)
    verify(macPosition.x + mac.width <= phonePosition.x)
    verify(phonePosition.x + phone.width <= seat.width)
    verify(macPosition.y + mac.height < seat.ellipseTop)
    verify(phonePosition.y + phone.height < seat.ellipseTop)
    var macLabel = mac.children[1]
    var phoneLabel = phone.children[1]
    function glyphEdge(label, atEnd) {
      var inset = (label.width - label.contentWidth) / 2
      return label.mapToItem(seat, atEnd ? label.width - inset : inset, 0).x
    }
    var labelGap = glyphEdge(phoneLabel, false) - glyphEdge(macLabel, true)
    verify(labelGap + 0.5 >= seat.labelGutter, "label gutter " + labelGap)
    function centerOf(node) {
      return node.mapToItem(seat, node.width / 2, 0).x
    }
    var macCenter = centerOf(mac)
    var phoneCenter = centerOf(phone)
    fuzzyCompare((macCenter + phoneCenter) / 2, seat.width / 2, 1)
    verify(Math.abs(centerOf(macLabel) - macCenter) < 1)
    verify(Math.abs(centerOf(phoneLabel) - phoneCenter) < 1)
    item.appleDevices = [phoneDevice]
    item.availableApple = [macDevice]
    item.linuxHeld = false
    tryCompare(seat, "occupantLabel", "iPhone")
    tryVerify(function () { return findChild(item, "waitingApple1") === null })
    compare(findChild(item, "waitingApple0").device.label, "MacBook Pro")
    fuzzyCompare(centerOf(findChild(item, "waitingApple0")), macCenter, 1)
  }

  // 2026-10-14: a tray Apple device can come forward only when the seated
  // Apple device is the idle audio owner: smart routing asks the owner to
  // yield, so a seated device that is not the owner (an Omarchy device
  // owns instead) cannot be taken from, and a playing owner is protected.
  function test_waitingMacLightsOnlyWhileSeatedPhoneIsNotPlaying() {
    var phone = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple", onAcl: true }
    var mac = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple" }
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var item = createTemporaryObject(view, this, {
      appleDevices: [phone], availableApple: [mac],
      omarchyDevices: [gram], connectedToOmarchy: true, linuxHeld: true
    })
    function iconOf(delegate) {
      var found = null
      function walk(node) {
        if (!node || found) return
        if (node.hasOwnProperty("selected") && node.hasOwnProperty("strokeColor")) { found = node; return }
        for (var i = 0; i < node.children.length; ++i) walk(node.children[i])
      }
      walk(delegate)
      return found
    }
    wait(400)
    var seat = findChild(item, "appleSeat")
    // Nobody owns the audio: the seated phone cannot yield a takeover.
    verify(seat.waitingLit === false)
    compare(iconOf(findChild(item, "waitingApple0")).level, 0)
    // The phone owns the audio idle (e.g. after a reconnect): the Mac can
    // come forward.
    item.appleOwnsAudio = true
    item.activeDeviceAddress = phone.address
    wait(100)
    verify(seat.waitingLit === true)
    compare(iconOf(findChild(item, "waitingApple0")).level, 1)
    item.audioPlaying = true
    wait(100)
    verify(seat.waitingLit === false)
    compare(iconOf(findChild(item, "waitingApple0")).level, 0)
  }

  // 2026-10-14: the mirror case from the panel: the Mac holds the Apple
  // seat but the XPS owns the audio, so the iPhone in the tray is truly
  // disabled (dimmest). If the Mac were the idle owner, it could yield.
  function test_waitingPhoneStaysDimWhileSeatedMacIsNotTheOwner() {
    var mac = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple", onAcl: true }
    var phone = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple" }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, {
      appleDevices: [mac], availableApple: [phone],
      omarchyDevices: [xps], connectedToOmarchy: true,
      omarchyOwnsAudio: true, activeDeviceAddress: xps.address
    })
    wait(400)
    var seat = findChild(item, "appleSeat")
    verify(seat.waitingLit === false)
    item.appleOwnsAudio = true
    item.omarchyOwnsAudio = false
    item.activeDeviceAddress = mac.address
    wait(100)
    verify(seat.waitingLit === true)
  }

  // 2026-10-13: a lone seated phone draws at the lit tray brightness while
  // the other seat owns the audio, without claiming the audio glow.
  function test_loneSeatedPhoneDrawsAtTrayBrightnessWhileOtherSeatOwns() {
    var phone = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, {
      appleDevices: [phone], omarchyDevices: [xps],
      omarchyOwnsAudio: true, activeDeviceAddress: xps.address
    })
    wait(200)
    var icon = findChild(item, "deviceApple0")
    compare(icon.level, 1, "idle seated phone draws at the active level while Omarchy owns the audio")
    verify(!icon.selected, "active is not ownership brightness")
    verify(icon.parent.parent.lit === false, "but the audio glow stays with the holder")
    item.omarchyOwnsAudio = false
    item.activeDeviceAddress = ""
    wait(100)
    compare(icon.level, 1, "a seated phone stays active even with no owner")
    item.appleDevices = [Object.assign({}, phone, { onAcl: false })]
    wait(100)
    compare(icon.level, 0, "off the link it dims to disabled")
  }

  // 2026-10-13: a displaced phone rises to the right-hand tray slot and the
  // device already waiting (the Mac) takes the left, even when both enter
  // the tray in the same update. Alone, the phone waits on the left.
  function test_displacedPhoneTakesRightTraySlot() {
    var phone = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple", onAcl: true }
    var mac = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple" }
    var item = createTemporaryObject(view, this, {
      appleDevices: [phone], availableApple: [mac], connectedToOmarchy: true
    })
    wait(700)
    item.appleDevices = []
    item.availableApple = [mac, Object.assign({}, phone, { onAcl: false })]
    wait(900)
    compare(findChild(item, "waitingApple0").device.label, "MacBook Pro")
    compare(findChild(item, "waitingApple1").device.label, "iPhone")
  }

  function test_displacedPhoneAloneWaitsOnTheLeft() {
    var phone = { address: "02:00:00:00:00:05", label: "iPhone", kind: "apple", onAcl: true }
    var item = createTemporaryObject(view, this, {
      appleDevices: [phone], availableApple: [], connectedToOmarchy: true
    })
    wait(700)
    item.appleDevices = []
    item.availableApple = [Object.assign({}, phone, { onAcl: false })]
    wait(900)
    var seat = findChild(item, "appleSeat")
    var waiting = findChild(item, "waitingApple0")
    compare(waiting.device.label, "iPhone")
    verify(waiting.mapToItem(item, waiting.width / 2, 0).x < seat.mapToItem(item, seat.width / 2, 0).x,
      "a lone displaced phone waits on the left")
  }

  // 2026-10-06: with the Mac seated, the iPhone vanished from the Apple tray whenever Omarchy held the audio or
  // ownership read as unknown. A waiting device stays in the tray, as lg-gram does on the Omarchy side.
  function test_appleWaitingDeviceStaysAboveConnectedSeat_data() {
    return [{ tag: "local audio", held: true, unknown: false },
      { tag: "remote audio", held: false, unknown: false },
      { tag: "unknown ownership", held: false, unknown: true }]
  }

  function test_appleWaitingDeviceStaysAboveConnectedSeat(data) {
    var item = createTemporaryObject(view, this, {
      appleDevices: [{ address: "02:00:00:00:00:06", label: "Mac", onAcl: true }],
      availableApple: [{ address: "02:00:00:00:00:05", label: "iPhone", kind: "apple" }],
      linuxHeld: data.held, ownershipUnknown: data.unknown, connectedToOmarchy: true
    })
    var seat = findChild(item, "appleSeat")
    verify(seat.hasWaitingDevice)
    tryVerify(function () { return findChild(item, "waitingApple0") !== null })
    compare(findChild(item, "waitingApple0").device.label, "iPhone")
  }

  // A fleet handoff reads "Switching…" in the empty Omarchy seat; FLEET stays a plain label.
  function test_switchingShowsInTheEmptyOmarchySeat() {
    var item = createTemporaryObject(view, this, { omarchyTransitionLabel: "Switching…" })
    var status = findChild(item, "seatStatusOmarchy"), seat = findChild(item, "omarchySeat")
    // 2026-10-07: full-strength text was too bright; it matches the dashed ellipse.
    tryCompare(status, "opacity", seat.ellipseOpacity, 1000)
    compare(status.color, seat.lineColor)
    compare(status.text, "Switching…")
    compare(findChild(item, "omarchySeatFleetLabel").text, "FLEET")
    verify(findChild(item, "omarchySeatSwitchingLabel") === null)
    compare(findChild(item, "seatStatusApple").opacity, 0)
    item.omarchyTransitionLabel = ""
    tryCompare(status, "opacity", 0, 1000)
  }

  function test_connectingShowsWhenTheOmarchySeatWasEmpty() {
    var item = createTemporaryObject(view, this, { omarchyTransitionLabel: "Connecting…" })
    var status = findChild(item, "seatStatusOmarchy"), seat = findChild(item, "omarchySeat")
    tryCompare(status, "opacity", seat.ellipseOpacity, 1000)
    compare(status.text, "Connecting…")
  }

  function test_switchingStaysOutOfAnOccupiedSeat() {
    var item = createTemporaryObject(view, this, { omarchyTransitionLabel: "Switching…",
      omarchyDevices: [{ address: "02:00:00:00:00:04", label: "omarchy-xps", kind: "omarchy", onAcl: true }] })
    wait(400)
    compare(findChild(item, "seatStatusOmarchy").opacity, 0)
  }

  // 2026-10-07: between a pair's two connectors the ellipse has a real gap rather than a dim arc. Shapes do not render
  // offscreen, so this checks the arc's ends land on the connectors' inner edges.
  function test_pairLeavesAGapInTheEllipse() {
    var gram = { address: "02:00:00:00:00:01", label: "lg-gram", onAcl: true }
    var xps = { address: "02:00:00:00:00:02", label: "omarchy-xps", onAcl: true }
    var item = createTemporaryObject(view, this, { connectedToOmarchy: true, omarchyOwnsAudio: true,
      omarchyDevices: [gram, xps], activeDeviceAddress: xps.address })
    wait(700)
    var seat = findChild(item, "omarchySeat"), arc = findChild(item, "seatOutlineOmarchy")
    verify(arc.sweepAngle < 360, "the outline leaves a gap")
    function endX(degrees) { return arc.radiusX * Math.cos(degrees * Math.PI / 180) }
    var first = endX(arc.startAngle), last = endX(arc.startAngle + arc.sweepAngle)
    fuzzyCompare(first, -seat.pairJoinGutter / 2, 0.01)
    fuzzyCompare(last, seat.pairJoinGutter / 2, 0.01)
    verify(Math.sin(arc.startAngle * Math.PI / 180) > 0.99, "the gap sits at the bottom, under the connectors")
    item.omarchyDevices = [xps]
    wait(400)
    compare(arc.sweepAngle, 360)
  }

  // 2026-10-07: beside a laptop, an Apple device can take the pods only from an Apple owner, so the tray hides otherwise.
  function test_appleTrayHidesWhenTheMacCannotTakeOver() {
    var phone = { address: "02:00:00:00:00:03", label: "iPhone", onAcl: true }
    var mac = { address: "02:00:00:00:00:06", label: "Mac", kind: "apple" }
    var item = createTemporaryObject(view, this, { appleDevices: [phone], availableApple: [mac], appleTrayAvailable: false })
    verify(!findChild(item, "appleSeat").hasWaitingDevice)
    item.appleTrayAvailable = true
    verify(findChild(item, "appleSeat").hasWaitingDevice)
  }

  // A non-playing phone displaced by this laptop rises to the tray and
  // stays there; the Mac already waiting fades in once the phone lands.
  function test_displacedPhoneRisesToTheTrayAndTheMacFadesIn() {
    var phone = { address: "02:00:00:00:00:03", label: "iPhone", onAcl: true }
    var mac = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple" }
    var item = createTemporaryObject(view, this, {
      appleDevices: [phone], appleTrayAvailable: false, connectedToOmarchy: true
    })
    wait(700)
    item.appleDevices = []
    item.omarchyDevices = [
      { address: "02:00:00:00:00:01", label: "omarchy-xps", onAcl: true },
      { address: "02:00:00:00:00:02", label: "lg-gram", onAcl: true }
    ]
    item.availableApple = [phone, mac]
    item.appleTrayAvailable = true
    wait(100)
    var rising = findChild(item, "waitingApple1")
    verify(rising, "the displaced phone appears in the tray")
    compare(rising.children[1].text, "iPhone")
    verify(rising.glideY > 5 || rising.glideX !== 0, "it glides from the seat, glide=" + rising.glideX + "," + rising.glideY)
    var waitingMac = findChild(item, "waitingApple0")
    compare(waitingMac.children[1].text, "MacBook Pro")
    verify(waitingMac.opacity < 0.5, "the Mac waits for the phone to land, opacity=" + waitingMac.opacity)
    tryCompare(rising, "glideY", 0, 1500)
    verify(rising.opacity > 0.4, "the phone stays in the tray, opacity=" + rising.opacity)
    tryCompare(waitingMac, "opacity", 1, 1500)
  }

  // Reconnect keeps both tray devices in place while it runs; the one that
  // takes the seat glides down from the tray and the other stays waiting.
  // 2026-10-14: "Reconnecting…" is an in-progress label like "Waiting…":
  // it takes the same dim treatment (seat colour at ellipse opacity), and
  // neither seat is clickable while a reconnect runs.
  function test_reconnectingLabelIsDimAndSeatsAreNotClickable() {
    var phone = { address: "02:00:00:00:00:03", label: "iPhone", kind: "apple" }
    var item = createTemporaryObject(view, this, {
      appleDevices: [], availableApple: [phone], connectedToOmarchy: true
    })
    wait(200)
    var action = findChild(item, "seatActionApple")
    verify(action.visible)
    item.reconnectBusy = true
    wait(100)
    compare(action.text, "Reconnecting…")
    var seat = findChild(item, "appleSeat")
    verify(Qt.colorEqual(action.color, seat.lineColor), "same colour as the Waiting… label")
    fuzzyCompare(action.opacity, seat.ellipseOpacity, 0.01)
    verify(!findChild(item, "seatClickApple").enabled, "Apple seat not clickable during reconnect")
    verify(!findChild(item, "seatClickOmarchy").enabled, "Omarchy seat not clickable during reconnect")
    item.reconnectBusy = false
    wait(100)
    compare(action.text, "Reconnect")
    fuzzyCompare(action.opacity, 1, 0.01)
    verify(findChild(item, "seatClickApple").enabled, "seats clickable again afterwards")
  }

  // 2026-10-14: while "Reconnecting…" runs, either Apple tray device
  // might take the seat, so both draw at the active (not owner) level
  // instead of the dimmest one.
  function test_reconnectLightsAppleTrayDevicesAtActiveLevel() {
    var phone = { address: "02:00:00:00:00:03", label: "iPhone", kind: "apple" }
    var mac = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple" }
    var item = createTemporaryObject(view, this, {
      appleDevices: [], availableApple: [phone, mac], appleTrayAvailable: true, connectedToOmarchy: true
    })
    function iconOf(delegate) {
      var found = null
      function walk(node) {
        if (!node || found) return
        if (node.hasOwnProperty("selected") && node.hasOwnProperty("strokeColor")) { found = node; return }
        for (var i = 0; i < node.children.length; ++i) walk(node.children[i])
      }
      walk(delegate)
      return found
    }
    wait(400)
    var seat = findChild(item, "appleSeat")
    verify(seat.waitingLit === false)
    compare(iconOf(findChild(item, "waitingApple0")).level, 0)
    compare(iconOf(findChild(item, "waitingApple1")).level, 0)
    item.reconnectBusy = true
    wait(100)
    verify(seat.waitingLit === true)
    compare(iconOf(findChild(item, "waitingApple0")).level, 1)
    compare(iconOf(findChild(item, "waitingApple1")).level, 1)
    verify(!iconOf(findChild(item, "waitingApple0")).selected, "active is not ownership brightness")
    item.reconnectBusy = false
    wait(100)
    verify(seat.waitingLit === false)
    compare(iconOf(findChild(item, "waitingApple0")).level, 0)
  }

  function test_reconnectHoldsTheTrayUntilADeviceDescends() {
    var phone = { address: "02:00:00:00:00:03", label: "iPhone", kind: "apple" }
    var mac = { address: "02:00:00:00:00:06", label: "MacBook Pro", kind: "apple" }
    var item = createTemporaryObject(view, this, {
      appleDevices: [], availableApple: [phone, mac], appleTrayAvailable: true, connectedToOmarchy: true
    })
    wait(700)
    verify(findChild(item, "waitingApple0"))
    verify(findChild(item, "waitingApple1"))
    item.activateApple()
    item.reconnectBusy = true
    // The status that starts reconnect clears the tray's data; the icons stay.
    item.availableApple = []
    item.appleTrayAvailable = false
    wait(100)
    verify(findChild(item, "waitingApple0"), "tray devices stay during reconnect")
    verify(findChild(item, "waitingApple1"), "tray devices stay during reconnect")
    // The phone takes the seat: it descends from the tray, the Mac remains.
    item.appleDevices = [{ address: phone.address, label: phone.label, onAcl: true }]
    wait(60)
    var seated = findChild(item, "deviceApple0").parent.parent
    verify(seated.glideY < -5, "the phone descends from the tray, glideY=" + seated.glideY)
    compare(findChild(item, "waitingApple0").children[1].text, "MacBook Pro")
    tryCompare(seated, "glideY", 0, 1500)
  }

  function test_discoveredLaptopWaitsOnOmarchySide_data() {
    return [{ tag: "local audio", held: true, unknown: false },
      { tag: "remote audio", held: false, unknown: false },
      { tag: "unknown ownership", held: false, unknown: true }]
  }

  function test_discoveredLaptopWaitsOnOmarchySide(data) {
    var item = createTemporaryObject(view, this, {
      availableOmarchy: [{ address: "02:00:00:00:00:04", label: "omarchy-xps", kind: "omarchy", discovered: true }],
      linuxHeld: data.held, ownershipUnknown: data.unknown, connectedToOmarchy: true
    })
    var seat = findChild(item, "omarchySeat")
    verify(seat !== null)
    verify(seat.hasWaitingDevice)
    compare(seat.waiterLabel, "omarchy-xps")
    var button = findChild(item, "fleetEnrollButton")
    verify(button !== null)
    verify(button.visible)
    compare(button.text, "+ Add to fleet")
    var waiting = findChild(item, "waitingOmarchy0")
    var label = waiting.children[1]
    verify(button.y >= label.y + label.height + waiting.spacing)
    var position = button.mapToItem(item, button.width / 2, button.height)
    verify(position.x > item.width / 2)
    verify(position.y <= seat.ellipseTop - seat.tallestSeatedRise - seat.waitingGap, "button bottom=" + position.y + " clearance=" + (seat.ellipseTop - seat.tallestSeatedRise - seat.waitingGap))
    verify(waiting.mapToItem(item, 0, 0).y >= 0)
  }

  function test_keyboardUsesTheDisplayedAction_data() {
    return [
      { tag: "reconnect", connected: false, busy: false, enabled: true, local: true, reconnects: 1, releases: 0 },
      { tag: "busy", connected: false, busy: true, enabled: true, local: true, reconnects: 0, releases: 0 },
      { tag: "disabled", connected: false, busy: false, enabled: false, local: true, reconnects: 0, releases: 0 },
      { tag: "local disconnected", connected: false, busy: false, enabled: true, local: false, reconnects: 0, releases: 0 },
      { tag: "release", connected: true, busy: false, enabled: true, local: true, reconnects: 0, releases: 1 }
    ]
  }

  function test_keyboardUsesTheDisplayedAction(data) {
    var item = createTemporaryObject(view, this, {
      appleDevices: [{ label: "iPhone", onAcl: data.connected }],
      connectedToOmarchy: data.local, reconnectBusy: data.busy, reconnectEnabled: data.enabled
    })
    reconnect.target = item
    release.target = item
    reconnect.clear()
    release.clear()
    item.activateApple()
    compare(reconnect.count, data.reconnects)
    compare(release.count, data.releases)
  }

  function test_disconnectedPhoneWaitsAboveReconnectButton() {
    var item = createTemporaryObject(view, this, {
      appleDevices: [{ label: "iPhone", onAcl: false }], connectedToOmarchy: true
    })
    var seat = findChild(item, "appleSeat")
    compare(seat.occupant, null)
    compare(seat.waiterLabel, "iPhone")
    verify(seat.hasWaitingDevice)
    compare(seat.actionLabel, "Reconnect")
    reconnect.target = item
    release.target = item
    reconnect.clear()
    release.clear()
    mouseClick(seat, seat.width / 2, seat.ellipseCenter)
    compare(reconnect.count, 1)
    compare(release.count, 0)
    item.reconnectBusy = true
    compare(seat.actionLabel, "Reconnecting…")
    mouseClick(seat, seat.width / 2, seat.ellipseCenter)
    compare(reconnect.count, 1)
  }

  function test_reconnectWaitsForRoster_data() {
    return [{ tag: "phone joined", joined: true }, { tag: "phone absent", joined: false }]
  }

  function test_reconnectWaitsForRoster(data) {
    var phone = "02:00:00:00:00:02"
    var status = Object.assign(Model.defaultStatus(), {
      connected: true, audioOwner: { holder: "local", playing: false },
      localAddress: "02:00:00:00:00:01", localName: "Laptop", remoteSourceAddress: phone,
      devices: { "02:00:00:00:00:02": { label: "Phone", kind: "apple" } }
    })
    var previous = Model.connectionPresentation(status, null, 1000)
    var item = createTemporaryObject(view, this, {
      appleDevices: previous.lastKnown.seats.apple, connectedToOmarchy: true, reconnectBusy: true
    })
    var seat = findChild(item, "appleSeat")
    status.audioOwner = { holder: "", playing: false }
    var pending = Model.connectionPresentation(status, previous, 2000)
    item.appleDevices = pending.snapshot.seats.apple
    compare(seat.actionLabel, "Reconnecting…")
    status.peerListUpdated = 2100
    status.connectedPeers = data.joined ? [phone] : []
    var confirmed = Model.connectionPresentation(status, pending, 2200)
    item.appleDevices = confirmed.snapshot.seats.apple
    item.reconnectBusy = false
    compare(seat.actionLabel, data.joined ? "" : "Reconnect")
    compare(seat.linkConnected, data.joined)
  }

  function test_connectedPhoneKeepsItsSeat() {
    var item = createTemporaryObject(view, this, {
      appleDevices: [{ label: "iPhone", onAcl: true }], connectedToOmarchy: true
    })
    var seat = findChild(item, "appleSeat")
    compare(seat.occupantLabel, "iPhone")
    compare(seat.actionLabel, "")
    reconnect.target = item
    release.target = item
    reconnect.clear()
    release.clear()
    mouseClick(seat, seat.width / 2, seat.ellipseCenter)
    compare(reconnect.count, 0)
    compare(release.count, 1)
  }
}
