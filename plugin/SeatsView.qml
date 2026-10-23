import QtQuick
import QtQuick.Shapes
import QtQuick.Controls
import "Model.js" as Model

Item {
  id: root

  /// The seat presentation, owned by Model's seatPresentation: the one
  /// input the panel forwards. Every property below binds from it; tests
  /// and other callers may still assign the properties directly, which
  /// simply replaces the binding for that property.
  property var presentation
  readonly property var presentationSeats: presentation && presentation.seats
    ? presentation.seats : { apple: [], omarchy: [] }
  property var appleDevices: presentationSeats.apple
  property var omarchyDevices: presentationSeats.omarchy
  property var availableApple: presentation ? presentation.availableApple : []
  property var availableOmarchy: presentation ? presentation.availableOmarchy : []
  property bool linuxHeld: presentation ? presentation.linuxHeld : false
  property bool ownershipUnknown: presentation ? presentation.viewOwnershipUnknown : false
  property bool connectedToOmarchy: presentation ? presentation.connectedToOmarchy : false
  property bool audioPlaying: presentation ? presentation.audioPlaying : false
  property string omarchyTransitionLabel: presentation ? presentation.omarchyTransitionLabel : ""
  property bool appleTrayAvailable: presentation ? presentation.appleTrayAvailable : true
  property bool appleSeatUnknown: presentation ? presentation.appleSeatUnknown === true : false
  property bool appleTrayRemembered: presentation ? presentation.appleTrayRemembered === true : false
  readonly property bool rememberedAppleTray: appleSeatUnknown || appleTrayRemembered
  property bool appleOwnsAudio: presentation ? presentation.appleOwnsAudio : false
  property bool omarchyOwnsAudio: presentation ? presentation.omarchyOwnsAudio : false
  property string activeDeviceAddress: presentation ? presentation.audioDeviceAddress : ""
  property string localAddress: ""
  /// Pair and tray orders the panel saved, keyed by seat name:
  /// { omarchy: { pair: [key, key], tray: [key, key] }, apple: ... }, where
  /// pair is the seat's last pair order (lastPairOrder). After a
  /// restart nothing has arrived or departed yet, so these keep each device
  /// on the side it was last drawn instead of falling back to address order.
  property var savedOrders: ({})
  signal orderDrawn(string seatName, string slot, var keys)
  function restoredOrder(saved, keys) {
    return saved && saved.length === 2 && keys.length === 2
      && keys.every(function (key) { return saved.indexOf(key) >= 0 }) ? saved.slice() : null
  }
  function sameOrder(a, b) {
    return !!a && !!b && a.length === b.length && a.every(function (key, at) { return b[at] === key })
  }
  property color foreground: "#e6e6e6"
  property color accent: "#7aa2f7"
  property color dim: "#888888"
  property color screenFill: "#1a1a1e"
  property string fontFamily: ""
  property string podsVariant: "buds"
  property string caseState: ""
  property bool pairingMode: false
  property int leftBatteryLevel: -1
  property int rightBatteryLevel: -1
  property int caseBatteryLevel: -1
  property int headsetBatteryLevel: -1
  property color lowBatteryColor: "#f7768e"
  // The case level only arrives through a docked bud, so both buds reporting from outside it leaves it unknown.
  readonly property bool caseNeedsDockedBud: leftBatteryLevel >= 0 && rightBatteryLevel >= 0 && caseBatteryLevel < 0
  readonly property bool showBattery: podsVariant === "max" ? headsetBatteryLevel >= 0
    : leftBatteryLevel >= 0 || rightBatteryLevel >= 0 || caseBatteryLevel >= 0
  property bool connectionSpinVisible: false
  property bool connectionComplete: false
  /// The one connection attempt, typed by Model: none, waiting,
  /// connecting, reconnecting, or switching. The transition label is
  /// display-only and never read back as state.
  property string attemptKind: presentation && presentation.attempt ? presentation.attempt.kind : "none"
  // The connector finishes independently of the status-bar ring. A quick
  // background connection still draws the remaining wire before fading.
  property int connectorPhase: 0 // idle, drawing, completing
  property real connectorOpacity: 0
  readonly property bool connectionAnimating: connectorPhase !== 0
  /// The attempt is live: the trace is running, or the attempt kind says
  /// this laptop is connecting (the labelled gap before the spin starts,
  /// and after it ends while a seat frees up).
  readonly property bool attemptActive: connectionAnimating
    || attemptKind === "connecting" || attemptKind === "reconnecting"
  // An attempt that began as a seat switch never draws a trace, even if
  // it stops being a switch partway through.
  property bool attemptQuiet: false

  property bool reconnectBusy: false
  property bool reconnectEnabled: true
  property bool fleetWaitingForKeyring: false

  signal enrollClicked(string address, string name, bool peerEnrolled)
  signal reconnectClicked
  signal appleClicked
  signal omarchyClicked

  function activateApple() {
    if (root.reconnectBusy || root.rememberedAppleTray) return
    if (appleSeat.actionLabel) {
      if (appleSeat.actionEnabled) { appleSeat.holdTray(); root.reconnectClicked() }
    } else if (root.connectedToOmarchy) root.appleClicked()
  }

  readonly property real seatShare: 0.46
  readonly property int budsSize: buds.usesPixelArt ? 128 : 56
  readonly property int caseGap: 8
  readonly property real artworkY: root.linkY + root.captionGap + root.caseGap
  // Lay the wires out for the incoming pods as soon as the case exits.
  // Hide the part covered by the moving lid, including the gaps between dots.
  readonly property real connectorEndY: buds.y
    + buds.connectorTop(root.budsSize, root.artworkY - buds.y)
  readonly property real revealedConnectorEndY: buds.y
    + buds.revealedConnectorTop(root.budsSize, root.artworkY - buds.y)
  readonly property int connectorDrawMs: 8000
  readonly property int wireGap: 10
  readonly property int moveMs: 450
  // A tray neighbour starts into the vacated slot shortly after the device
  // taking the seat has dropped clear of the tray row, so the two move
  // together like a rotation without crossing.
  readonly property int traySwapStaggerMs: 25
  // Ends at the same instant as the move to the seat: stagger + slide = moveMs.
  readonly property int traySwapMoveMs: moveMs - traySwapStaggerMs
  readonly property int riseHorizontalMs: 495
  // Sweep outward while rising, then curl back into the tray near the top.
  readonly property var riseHorizontalCurve: [0.12, 0.5, 0.6, 1.45, 0.82, 1.45,
                                               0.9, 1.45, 0.93, 1, 1, 1]
  // Waiting devices draw smaller. A device crossing between a seat and the tray grows or shrinks with its glide.
  readonly property real waitingIconScale: 0.75
  readonly property int seatedLabelPx: 11
  readonly property int waitingLabelPx: 9
  readonly property int enrollButtonHeight: 20
  readonly property bool showEnrollment: availableOmarchy.some(function (device) { return device.discovered === true })
  // A device that stays in a seat or the tray holds its spot until a leaving neighbour has glided away.
  readonly property int recenterDelayMs: moveMs
  QtObject {
    id: motionHistory
    readonly property int lifetimeMs: 1000
    property var placements: ({})
    property var seatedSpots: ({})
    property var departedAt: ({})
    property var departed: ({})
    property int departures: 0
    function capture(address, point, seated, iconScale, labelSize) {
      if (address) placements[address] = { x: point.x, y: point.y, seated: seated,
        iconScale: iconScale, labelSize: labelSize, at: Date.now() }
    }
    function departure(address, point, iconScale, labelSize, label) {
      if (!address) return
      if (seatedSpots[address]) seatedSpots[address].leftAt = Date.now()
      capture(address, point, true, iconScale, labelSize)
      departedAt[address] = Date.now()
      departed[address] = { address: address, label: label || "", at: Date.now() }
      ++departures
    }
    function rest(address, point) {
      if (address) seatedSpots[address] = { x: point.x, y: point.y, seated: true }
    }
    function seatedOrigin(address) {
      const rest = seatedSpots[address]
      return rest && (!rest.leftAt || Date.now() - rest.leftAt < lifetimeMs) ? rest : null
    }
    function origin(address, seated, slotFrom) {
      const saved = placements[address]
      const placed = saved && Date.now() - saved.at < lifetimeMs ? saved : null
      return seated ? (placed && !placed.seated ? placed : slotFrom || placed) : placed
    }
  }
  property real connectionProgress: 0
  readonly property int seatsHeight: Math.ceil(Math.max(120, appleSeat.waitingClearance, omarchySeat.waitingClearance))
  readonly property real lowerLabelBottom: Math.max(
    appleSeat.titleBottom,
    omarchySeat.titleBottom
  )
  property real captionGap: 12
  readonly property real linkY: root.lowerLabelBottom + root.captionGap
  readonly property real artworkBottom: root.showBattery ? batteryRow.y + batteryRow.height : buds.y + buds.height
  implicitHeight: artworkBottom + captionGap

  SeatChannel {
    id: appleSeat
    objectName: "appleSeat"
    title: "Apple"
    orderName: "apple"
    waitingUnconfirmed: root.rememberedAppleTray
    // A tray Apple device can come forward by smart routing only when the
    // seated Apple device is the idle audio owner: the routing request
    // asks the owner to yield. A seated device that is not the owner (an
    // Omarchy device owns instead) cannot be taken from, and a playing
    // owner is protected, so the tray device stays dim in those states.
    // During a reconnect either tray device might take the seat, so
    // both draw at the active (not owner) brightness while it runs.
    waitingLit: root.reconnectBusy || (!!occupant && occupantOnAcl && !root.audioPlaying && Model.seatDeviceLit(
      { address: occupant.address, onAcl: occupantOnAcl }, layout, ownsAudio))
    layout: Model.appleSeat(root.appleDevices, root.availableApple, root.activeDeviceAddress,
      { busy: root.reconnectBusy, connected: root.connectedToOmarchy, enabled: root.reconnectEnabled }, root.appleTrayAvailable, root.rememberedAppleTray)
    ownsAudio: root.appleOwnsAudio
    otherOwnsAudio: root.omarchyOwnsAudio
    clickSuspended: root.reconnectBusy
    lineColor: root.foreground
    dimColor: root.dim
    screenFill: root.screenFill
    fontFamily: root.fontFamily
    width: parent.width * root.seatShare
    height: root.seatsHeight
    x: 0
    onClicked: root.activateApple()
  }

  SeatChannel {
    id: omarchySeat
    objectName: "omarchySeat"
    title: "Omarchy"
    orderName: "omarchy"
    statusText: root.omarchyTransitionLabel
    switching: root.attemptKind === "switching"
    connecting: root.attemptActive
    layout: Model.omarchySeat(root.omarchyDevices, root.availableOmarchy, root.activeDeviceAddress,
      root.connectedToOmarchy, root.linuxHeld, root.ownershipUnknown, root.localAddress)
    ownsAudio: root.omarchyOwnsAudio
    otherOwnsAudio: root.appleOwnsAudio
    reconnectSeatGuard: true
    waitingAlwaysActive: true
    clickSuspended: root.reconnectBusy
    lineColor: root.foreground
    dimColor: root.dim
    screenFill: root.screenFill
    fontFamily: root.fontFamily
    width: parent.width * root.seatShare
    height: root.seatsHeight
    x: parent.width - width
    onClicked: root.omarchyClicked()
  }

  SeatLink {
    seat: appleSeat
    towardCenter: 1
  }

  SeatLink {
    id: omarchyLink
    objectName: "omarchyLink"
    seat: omarchySeat
    towardCenter: -1
    connecting: root.connectionAnimating
  }

  AirPodsIcon {
    id: buds
    objectName: "seatPodsArtwork"
    z: 4
    // 5% larger, rounded to a whole pixel; the dot grid stays physical.
    iconSize: usesMeshCase ? 137 : root.budsSize
    pixelArt: true
    color: root.foreground
    variant: root.podsVariant
    caseState: root.caseState
    pairingMode: root.pairingMode
    // The stems end at y=113 and the case at y=117.5. Keep their painted
    // bottoms on the same baseline beside the case battery circle.
    y: root.artworkY + (usesMeshCase ? -4.5 : 0)
    anchors.horizontalCenter: parent.horizontalCenter
  }

  Row {
    id: batteryRow
    objectName: "podBatteryIndicators"
    visible: root.showBattery
    anchors.horizontalCenter: buds.horizontalCenter
    // Keep the gauges fixed while the pods drop into place.
    y: root.artworkY + root.budsSize + 4
    spacing: 16
    BatteryIndicator {
      objectName: "leftBatteryIndicator"
      visible: root.podsVariant !== "max"
      width: (Math.max(128, root.budsSize) - batteryRow.spacing) / 2
      height: implicitHeight
      label: "L"
      level: root.leftBatteryLevel
      foreground: root.foreground
      lowColor: root.lowBatteryColor
      fontFamily: root.fontFamily
    }
    BatteryIndicator {
      objectName: "rightBatteryIndicator"
      visible: root.podsVariant !== "max"
      width: (Math.max(128, root.budsSize) - batteryRow.spacing) / 2
      height: implicitHeight
      label: "R"
      level: root.rightBatteryLevel
      foreground: root.foreground
      lowColor: root.lowBatteryColor
      fontFamily: root.fontFamily
    }
    BatteryIndicator {
      visible: root.podsVariant === "max"
      height: implicitHeight
      width: implicitWidth
      level: root.headsetBatteryLevel
      foreground: root.foreground
      lowColor: root.lowBatteryColor
      fontFamily: root.fontFamily
    }
  }

  Column {
    id: caseBattery
    objectName: "caseBatteryIndicator"
    visible: root.showBattery && root.podsVariant !== "max"
    // Follow the case's 3 px wider painted right edge, preserving the gutter.
    x: buds.x + buds.width / 2 + Math.max(128, root.budsSize) / 2 + 27
    y: (root.artworkY + root.artworkBottom - height) / 2
    width: 64
    spacing: 8
    Item {
      anchors.horizontalCenter: parent.horizontalCenter
      width: 32
      height: 28
      Rectangle {
        anchors.centerIn: parent
        width: 32; height: 24; radius: 7
        color: "transparent"
        border.color: root.dim
        border.width: 1.25
        Rectangle { x: 1; y: 8; width: 30; height: 1; color: root.dim }
        Rectangle { anchors.horizontalCenter: parent.horizontalCenter; y: 13; width: 2; height: 2; radius: 1; color: root.dim }
      }
    }
    BatteryIndicator {
      objectName: "caseBatteryLevel"
      anchors.horizontalCenter: parent.horizontalCenter
      width: implicitWidth
      height: implicitHeight
      label: "CASE"
      level: root.caseBatteryLevel
      unknownCaption: root.caseNeedsDockedBud ? "Dock a bud" : ""
      foreground: root.foreground
      lowColor: root.lowBatteryColor
      fontFamily: root.fontFamily
    }
  }
  Rectangle {
    visible: caseBattery.visible
    x: caseBattery.x - 12
    y: caseBattery.y
    width: 1
    height: caseBattery.height
    color: Qt.alpha(root.foreground, 0.25)
  }

  Shape {
    objectName: "connectionTrace"
    anchors.fill: parent
    preferredRendererType: Shape.CurveRenderer
    visible: root.connectionAnimating
    opacity: root.connectorOpacity
    ShapePath {
      strokeColor: root.foreground
      strokeWidth: 1.25
      capStyle: ShapePath.RoundCap
      fillColor: "transparent"
      trim.end: root.connectionProgress
      startX: omarchyLink.wireX
      startY: root.revealedConnectorEndY
      PathLine { x: omarchyLink.wireX; y: root.linkY + omarchyLink.cornerRadius }
      PathQuad {
        x: omarchyLink.wireX + omarchyLink.cornerRadius
        y: root.linkY
        controlX: omarchyLink.wireX
        controlY: root.linkY
      }
      PathLine { x: omarchySeat.titleCenterX - omarchyLink.cornerRadius; y: root.linkY }
      PathQuad {
        x: omarchySeat.titleCenterX
        y: root.linkY - omarchyLink.cornerRadius
        controlX: omarchySeat.titleCenterX
        controlY: root.linkY
      }
      PathLine { x: omarchySeat.titleCenterX; y: omarchySeat.titleBottom }
    }
  }

  // The attempt machine. One reconcile folds the attempt kind, the ring
  // spin, and the completion fact into the trace phase:
  // - waiting: not this laptop's attempt yet; no trace, however long the
  //   ring spins. It plays once waiting ends.
  // - switching from the start: quiet for the whole attempt (the wire
  //   stays solid and the switch is drawn by the gliding devices).
  // - switching mid-draw: the trace is not interrupted; it finishes.
  // - connecting/reconnecting: the trace sweeps until the device lands,
  //   even after the spin ends while a seat frees up.
  // - complete after the spin: the remaining wire draws fast, holds,
  //   and fades (the finishing animation), then the seat takes over.
  function resetTrace() {
    drawing.stop()
    finishing.stop()
    connectorPhase = 0
    connectorOpacity = 0
    connectionProgress = 0
  }
  function reconcileAttempt() {
    if (attemptKind === "waiting") {
      if (connectorPhase === 1) resetTrace()
      return
    }
    if (connectionSpinVisible) {
      if (attemptKind === "switching" && connectorPhase === 0) attemptQuiet = true
      if (attemptQuiet) return
      if (connectorPhase !== 1) {
        finishing.stop()
        connectorPhase = 1
        connectorOpacity = 1
        drawing.restart()
      }
      return
    }
    attemptQuiet = false
    if (connectionComplete && connectorPhase === 1) {
      drawing.stop()
      connectorPhase = 2
      finishing.restart()
    } else if (attemptKind !== "connecting" && attemptKind !== "reconnecting"
      && connectorPhase === 1) {
      resetTrace()
    }
  }
  // The kind and the two event flags arrive through different bindings.
  // Reconcile them together rather than hiding the trace between those
  // assignments.
  onConnectionSpinVisibleChanged: Qt.callLater(reconcileAttempt)
  onConnectionCompleteChanged: Qt.callLater(reconcileAttempt)
  onAttemptKindChanged: Qt.callLater(reconcileAttempt)
  Component.onCompleted: reconcileAttempt()

  NumberAnimation {
    id: drawing
    target: root
    property: "connectionProgress"
    from: 0
    to: 1
    duration: root.connectorDrawMs
    easing.type: Easing.InOutQuad
    loops: Animation.Infinite
  }

  SequentialAnimation {
    id: finishing
    NumberAnimation {
      target: root; property: "connectionProgress"; to: 1
      duration: root.moveMs; easing.type: Easing.OutCubic
    }
    PauseAnimation { duration: 200 }
    NumberAnimation { target: root; property: "connectorOpacity"; to: 0; duration: 200 }
    onFinished: root.connectorPhase = 0
  }

  component SeatLink: Shape {
    id: branch
    required property Item seat
    required property real towardCenter
    property bool connecting: false
    readonly property bool solid: connecting || seat.connectorConnected
    readonly property real attachX: seat.titleCenterX
    readonly property real attachY: seat.titleBottom
    readonly property real cornerRadius: 8
    // Each seat has its own wire into the pods, side by side, so each connection reads on its own.
    readonly property real wireX: root.width / 2 - towardCenter * root.wireGap / 2
    anchors.fill: parent
    preferredRendererType: Shape.CurveRenderer
    readonly property real paintFactor: connecting ? 0.25 : seat.connectorConnected && !seat.drawnConnected ? 0.5 : seat.linkOpacity

    ShapePath {
      strokeColor: branch.seat.paint(branch.paintFactor)
      strokeWidth: branch.solid ? 1.25 : branch.seat.linkWidth
      strokeStyle: branch.solid ? ShapePath.SolidLine : ShapePath.DashLine
      dashPattern: [3, 4]
      capStyle: ShapePath.RoundCap
      fillColor: "transparent"
      startX: branch.attachX
      startY: branch.attachY
      PathLine { x: branch.attachX; y: root.linkY - branch.cornerRadius }
      PathQuad {
        x: branch.attachX + branch.towardCenter * branch.cornerRadius
        y: root.linkY
        controlX: branch.attachX
        controlY: root.linkY
      }
      PathLine { x: branch.wireX - branch.towardCenter * branch.cornerRadius; y: root.linkY }
      PathQuad {
        x: branch.wireX
        y: root.linkY + branch.cornerRadius
        controlX: branch.wireX
        controlY: root.linkY
      }
      PathLine { x: branch.wireX; y: root.revealedConnectorEndY }
    }
  }

  component SeatChannel: Item {
    id: seat
    required property var layout
    property string orderName: ""
    readonly property var savedOrder: root.savedOrders && root.savedOrders[orderName] || ({})
    readonly property string actionLabel: layout.actionLabel
    readonly property bool actionEnabled: layout.actionEnabled
    readonly property bool showWaiting: layout.showWaiting
    readonly property bool linkConnected: layout.linkConnected
    // Drawn dashed as soon as the link drops, and solid only once no device is still gliding in from the tray.
    readonly property bool arriving: firstPerson.arriving || secondPerson.arriving
    property bool drawnConnected: false
    property bool switching: false
    /// A connection attempt toward this seat is in flight: the seat
    /// reads as connecting (solid connector) rather than disconnected,
    /// while its status label names the attempt at the dim label
    /// treatment like "Waiting…".
    property bool connecting: false
    readonly property bool connectorConnected: drawnConnected || switching || connecting || (linkConnected && arriving)
    Component.onCompleted: {
      drawnConnected = linkConnected
      restoreSavedPair()
    }
    function holdTray() { waitingStack.captureHold() }
    function syncDrawn() { if (linkConnected && !arriving) drawnConnected = true }
    onLinkConnectedChanged: { if (!linkConnected) drawnConnected = false; else drawnSync.restart() }
    onArrivingChanged: drawnSync.restart()
    Timer { id: drawnSync; interval: 0; onTriggered: seat.syncDrawn() }
    property bool ownsAudio: false
    readonly property string audioDeviceAddress: layout.audioDeviceAddress
    readonly property string linkState: !drawnConnected ? "disconnected" : ownsAudio ? "claimed" : "connected"
    readonly property real linkOpacity: linkState === "claimed" ? 1 : drawnConnected ? 0.5 : 0.25
    readonly property real linkWidth: drawnConnected ? 1.25 : 1
    // Claimed audio brightens the connector at the same weight. The ellipse stays at the connected opacity.
    readonly property real ellipseOpacity: drawnConnected ? 0.3 : linkOpacity
    /// Shapes draw opaque in a colour blended from the line colour toward
    /// the background by the state's factor, instead of the line colour
    /// at that opacity: overlapping translucent strokes composite into a
    /// bright dot where they meet, while one opaque colour merges
    /// invisibly. Over this uniform background the blended colour is
    /// identical where strokes do not meet.
    function paint(factor) {
      return Qt.rgba(screenFill.r + (lineColor.r - screenFill.r) * factor,
        screenFill.g + (lineColor.g - screenFill.g) * factor,
        screenFill.b + (lineColor.b - screenFill.b) * factor, 1)
    }
    readonly property color ellipsePaint: paint(ellipseOpacity)
    readonly property real ellipseWidth: linkWidth
    property string title: ""
    property string statusText: ""
    property bool waitingUnconfirmed: false
    /// Tray devices drawn lit (active colour) rather than dim.
    property bool waitingLit: false
    /// Whether the other seat currently owns the audio.
    property bool otherOwnsAudio: false
    /// Tray devices can always come forward (Omarchy seat): they draw at
    /// the active brightness instead of waiting for a lit state.
    property bool waitingAlwaysActive: false
    /// During an Apple reconnect only this laptop may (re)enter this seat.
    property bool reconnectSeatGuard: false
    /// Clicks are suspended while a reconnect runs.
    property bool clickSuspended: false
    readonly property real titleCenterX: x + width / 2
    readonly property var devices: layout.devices
    readonly property var waiting: layout.waiting
    property color lineColor
    property color dimColor
    property color screenFill
    property string fontFamily: ""
    signal clicked

    readonly property real padW: width * 0.94
    readonly property int padH: 72
    readonly property int titleGap: 16
    readonly property real padY: height - titleGap - padH / 2 - padH * padFlat
    readonly property real padFlat: 0.36
    property var pairHold: ({ pair: [], emptied: false })
    /// The seat emptied during this reconnect (pairHold's flag is gone the
    /// moment a device is named again, so this one sticks until the pair
    /// reforms or the reconnect ends).
    property bool reconnectEmptied: false
    onDevicesChanged: {
      pairHold = Model.seatPairHold(pairHold, devices, root.reconnectBusy)
      if (devices.length === 2) reconnectEmptied = false
      else if (root.reconnectBusy && pairHold.emptied) reconnectEmptied = true
    }
    Connections {
      target: root
      function onReconnectBusyChanged() { if (!root.reconnectBusy) seat.reconnectEmptied = false }
    }
    // One binding, so a frame never pairs the new list with a stale hold and shifts the remaining laptop.
    // Once the seat has emptied mid-reconnect, a remembered snapshot can
    // name the other laptop as its occupant again. That laptop was asked
    // to stay off, so only this laptop may take the seat from the tray;
    // the other stays up there (the tray hold below keeps it drawn).
    readonly property var seated: {
      const list = Model.seatedDevices(devices, pairHold, root.reconnectBusy)
      if (!reconnectSeatGuard || !root.reconnectBusy || !reconnectEmptied) return list
      return list.filter(function (device) {
        return Model.normalizeAddress(device.address || "") === Model.normalizeAddress(root.localAddress)
      })
    }
    // Pair order is by arrival: when a second device joins, the newcomer
    // takes the left slot and the device already seated slides right,
    // whichever laptop is drawing the seat.
    property var pairOrder: []
    /// The seat order of the last pair, kept after the seat empties: the
    /// tray places the risen pair on the sides they sat on.
    property var lastPairOrder: []
    property var previousSeatedKeys: []
    /// Nothing this session has decided the pair order (no arrival, no
    /// break-up), so the saved last pair may still be restored.
    property bool pairOrderGuessed: true
    function restoreSavedPair() {
      const saved = savedOrder.pair
      if (!pairOrderGuessed || !saved || saved.length !== 2) return
      if (pairOrder.length === 2) {
        const restored = root.restoredOrder(saved, pairOrder)
        if (!restored) return
        pairOrder = restored
      }
      if (lastPairOrder.length === 0 || root.restoredOrder(saved, lastPairOrder)) lastPairOrder = saved.slice()
    }
    onSavedOrderChanged: restoreSavedPair()
    onLastPairOrderChanged: {
      if (orderName && !root.sameOrder(lastPairOrder, savedOrder.pair || []))
        root.orderDrawn(orderName, "pair", lastPairOrder.slice())
    }
    function seatedKey(device) { return Model.normalizeAddress(device && device.address || "") }
    onSeatedChanged: {
      const list = seated || []
      const keys = list.map(seatedKey)
      if (list.length === 2) {
        const samePair = pairOrder.length === 2
          && pairOrder.every(function (key) { return keys.indexOf(key) >= 0 })
        if (!samePair) {
          const fresh = list.filter(function (device) {
            return previousSeatedKeys.indexOf(seatedKey(device)) < 0
          })
          const restored = fresh.length === 1 ? null : root.restoredOrder(savedOrder.pair, keys)
          if (fresh.length === 1 || restored) pairOrderGuessed = false
          pairOrder = fresh.length === 1
            ? [seatedKey(fresh[0]), keys.find(function (key) { return key !== seatedKey(fresh[0]) })]
            : restored || keys.slice()
        }
        if (pairOrder.length === 2 && !root.sameOrder(pairOrder, lastPairOrder)) lastPairOrder = pairOrder.slice()
      } else {
        pairOrder = []
        // One member of the pair is back in the seat on its own: the tray
        // episode is over and the other re-centres to the lone left slot.
        if (list.length === 1 && !list[0].standIn && lastPairOrder.length === 2
          && lastPairOrder.indexOf(keys[0]) >= 0) {
          pairOrderGuessed = false
          lastPairOrder = []
        }
      }
      previousSeatedKeys = keys
    }
    readonly property var seatedOrdered: {
      if (!seated || seated.length !== 2 || pairOrder.length !== 2) return seated
      const byKey = {}
      seated.forEach(function (device) { byKey[seatedKey(device)] = device })
      return pairOrder.map(function (key) { return byKey[key] }).filter(function (device) { return !!device })
    }
    readonly property var occupant: seatedOrdered && seatedOrdered.length ? seatedOrdered[0] : null
    readonly property var occupantTwo: seatedOrdered && seatedOrdered.length > 1 ? seatedOrdered[1] : null
    // Whoever last held this seat takes the right-hand tray slot when they
    // leave it; devices already waiting keep the left. A lone device waits
    // on the left.
    property string lastOccupantAddress: ""
    onOccupantChanged: if (occupant) lastOccupantAddress = Model.normalizeAddress(occupant.address || "")
    // This laptop is taking the seat but has not landed yet: the seated
    // device slides to the right slot at once, so the takeover reads as
    // responsive and the left slot is already clear when it arrives.
    readonly property bool pairIncoming: switching && !occupantTwo && !!occupant
      && root.localAddress !== ""
      && Model.normalizeAddress(occupant.address || "") !== Model.normalizeAddress(root.localAddress)
      && (waiting || []).some(function (device) {
        return Model.normalizeAddress(device.address || "") === Model.normalizeAddress(root.localAddress)
      })
    readonly property var incomingDevice: (waiting || []).filter(function (device) {
      return Model.normalizeAddress(device.address || "") === Model.normalizeAddress(root.localAddress)
    })[0] || null
    readonly property string incomingLabel: incomingDevice ? String(incomingDevice.label || "") : ""
    readonly property var waiter: waiting && waiting.length ? waiting[0] : null
    readonly property string occupantLabel: occupant ? String(occupant.label) : "Empty"
    readonly property string occupantTwoLabel: occupantTwo ? String(occupantTwo.label) : ""
    readonly property bool occupantPhone: Model.isPhone(occupantLabel)
    readonly property bool occupantTwoPhone: Model.isPhone(occupantTwoLabel)
    readonly property bool occupantOnAcl: occupant ? occupant.onAcl !== false : false
    readonly property bool occupantTwoOnAcl: occupantTwo ? occupantTwo.onAcl !== false : false
    readonly property string waiterLabel: waiter ? String(waiter.label) : ""
    readonly property bool hasWaitingDevice: showWaiting && waiterLabel !== ""
    readonly property real ellipseRadiusX: padW * 0.48
    readonly property real ellipseCenter: padY + padH / 2
    readonly property real ellipseTop: ellipseCenter - padH * padFlat
    readonly property real ellipseBottom: ellipseCenter + padH * padFlat
    readonly property real titleBottom: seatTitle.y + seatTitle.height
    readonly property real titleTop: seatTitle.y
    readonly property int pairGap: 26
    readonly property int labelGutter: 16
    readonly property int iconOverlap: 14
    readonly property int waitingGap: 10
    // Both trays reserve the invitation row so their icons stay aligned.
    readonly property real tallestSeatedRise: 32.5 - iconOverlap
    readonly property real trayHeight: 32.5 * waitingStack.iconScale + 6 + Math.ceil(trayLabel.height)
      + (root.showEnrollment ? 6 + root.enrollButtonHeight : 0)
    readonly property real waitingClearance: titleGap + 2 * padH * padFlat + tallestSeatedRise + waitingGap + trayHeight
    readonly property real stackChipH: (occupantPhone || occupantTwoPhone)
      ? 32.5 : 22.5
    readonly property color ink: drawnConnected ? lineColor : dimColor

    FontMetrics {
      id: trayLabel
      font.family: seat.fontFamily
      font.pixelSize: 9
    }

    Item {
      id: waitingStack
      parent: seat.parent
      z: 2
      readonly property bool requested: seat.hasWaitingDevice || (holdingTray && heldDevices.length > 0)
      readonly property int revealDelay: 400
      readonly property int fadeDuration: 300
      readonly property real iconScale: root.waitingIconScale
      // Tray devices are held from the Reconnect click until reconnect ends.
      // One that takes the seat leaves the hold and glides down; the rest stay.
      property var heldDevices: []
      property bool trayHeld: false
      readonly property bool holdingTray: trayHeld || root.reconnectBusy
      function captureHold() {
        trayHeld = true
        var held = []
        for (var i = 0; i < waitingModel.count; ++i) held.push(waitingModel.get(i).device)
        heldDevices = held
        holdExpiry.restart()
        syncDevices()
      }
      Timer {
        id: holdExpiry
        interval: 30000
        onTriggered: if (!root.reconnectBusy) { waitingStack.trayHeld = false; waitingStack.heldDevices = []; waitingStack.syncDevices() }
      }
      Connections {
        target: root
        function onReconnectBusyChanged() {
          if (root.reconnectBusy) waitingStack.captureHold()
          else { waitingStack.trayHeld = false; waitingStack.heldDevices = []; waitingStack.syncDevices() }
        }
      }
      readonly property var shown: {
        motionHistory.departures
        let base = Model.trayDevices(seat.waiting, motionHistory.departedAt)
        if (holdingTray) {
          // Keys are the drawn seat, not the raw layout: a laptop the
          // reconnect guard keeps out of the seat stays in the tray.
          const seated = {}
          seat.seated.forEach(function (device) { seated[Model.normalizeAddress(device.address || "")] = true })
          const extras = []
          heldDevices.forEach(function (device) {
            const key = Model.normalizeAddress(device.address || "")
            if (!seated[key] && !base.some(function (d) { return Model.normalizeAddress(d.address || "") === key })
              && !extras.some(function (d) { return Model.normalizeAddress(d.address || "") === key }))
              extras.push(device)
          })
          base = base.concat(extras).slice(0, 2)
        }
        // Nothing has departed since a restart: keep the tray's saved sides.
        if (base.length === 2 && !(Model.normalizeAddress(base[0].address || "") in motionHistory.departedAt)
          && !(Model.normalizeAddress(base[1].address || "") in motionHistory.departedAt)) {
          const restored = root.restoredOrder(seat.savedOrder.tray,
            base.map(function (d) { return Model.normalizeAddress(d.address || "") }))
          if (restored)
            base.sort(function (a, b) {
              return restored.indexOf(Model.normalizeAddress(a.address || ""))
                - restored.indexOf(Model.normalizeAddress(b.address || ""))
            })
        }
        // A pair that rose from the seat keeps its seat sides in the tray,
        // whichever of them left first: left seat to left tray, right to right.
        if (base.length === 2 && seat.lastPairOrder.length === 2) {
          const order = {}
          seat.lastPairOrder.forEach(function (key, at) { order[key] = at })
          const first = Model.normalizeAddress(base[0].address || "")
          const second = Model.normalizeAddress(base[1].address || "")
          if (first in order && second in order)
            base.sort(function (a, b) {
              return order[Model.normalizeAddress(a.address || "")]
                - order[Model.normalizeAddress(b.address || "")]
            })
        }
        return base
      }
      onShownChanged: syncDevices()
      Component.onCompleted: syncDevices()
      // Fleet refreshes update each device in place so its active glide survives.
      function syncDevices() {
        const beforeKeys = []
        for (var b = 0; b < waitingModel.count; ++b) beforeKeys.push(waitingModel.get(b).key)
        // While the tray is held, a device already drawn here stays unless
        // the drawn seat takes it: an update whose waiting list empties (a
        // snapshot naming it seated again) must not drop it from the tray.
        const effective = shown.slice()
        if (holdingTray) {
          const seatedKeys = {}
          seat.seated.forEach(function (device) {
            seatedKeys[Model.normalizeAddress(device.address || "")] = true })
          // The drawn seat can lag a frame behind the guard, so retention
          // follows the guard rule itself: a non-local tray device never
          // leaves during a reconnect, and the local one leaves only once
          // it is drawn in the seat.
          const guardActive = seat.reconnectSeatGuard && root.reconnectBusy
          const localKey = Model.normalizeAddress(root.localAddress)
          for (var r = 0; r < waitingModel.count; ++r) {
            const item = waitingModel.get(r)
            const retain = guardActive
              ? item.key !== localKey || !seatedKeys[item.key]
              : !seatedKeys[item.key]
            if (retain && !effective.some(function (device) {
              return Model.normalizeAddress(device.address || "") === item.key }))
              effective.push(item.device)
          }
        }
        shrank = effective.length < waitingModel.count
        for (var i = waitingModel.count - 1; i >= 0; --i) {
          var key = waitingModel.get(i).key
          if (!effective.some(function (device) { return Model.normalizeAddress(device.address || "") === key })) {
            waitingDevices.itemAt(i).recordPlacement()
            waitingModel.remove(i)
          }
        }
        for (var at = 0; at < effective.length; ++at) {
          var device = effective[at]
          var key = Model.normalizeAddress(device.address || "")
          var found = -1
          for (var j = at; j < waitingModel.count; ++j)
            if (waitingModel.get(j).key === key) { found = j; break }
          if (found < 0) waitingModel.insert(at, { key: key, device: device })
          else {
            if (found !== at) waitingModel.move(found, at, 1)
            waitingModel.setProperty(at, "device", device)
          }
        }
        // A device displaced from the seat joins the right of a tray that
        // already has devices waiting; a pair rising together into an
        // empty tray (Reconnect) keeps its seat order instead.
        const last = seat.lastOccupantAddress
        const risenPair = seat.lastPairOrder.length === 2 && waitingModel.count === 2
          && seat.lastPairOrder.indexOf(waitingModel.get(0).key) >= 0
          && seat.lastPairOrder.indexOf(waitingModel.get(1).key) >= 0
        if (!risenPair && last !== "" && beforeKeys.length > 0 && beforeKeys.indexOf(last) < 0
          && last !== Model.normalizeAddress(seat.occupant && seat.occupant.address || "")) {
          for (var m = 0; m < waitingModel.count; ++m) {
            if (waitingModel.get(m).key === last && m < waitingModel.count - 1) {
              waitingModel.move(m, waitingModel.count - 1, 1)
              break
            }
          }
        }
        if (seat.orderName && waitingModel.count === 2) {
          const drawn = [waitingModel.get(0).key, waitingModel.get(1).key]
          if (!root.sameOrder(drawn, seat.savedOrder.tray)) root.orderDrawn(seat.orderName, "tray", drawn)
        }
      }
      ListModel { id: waitingModel; dynamicRoles: true }
      // Reserve both slots so a lone device keeps the left position of a centered pair.
      readonly property real slotWidth: (width - seat.labelGutter) / 2
      // A device rising from the seat skips the reveal delay so it stays visible while it moves.
      property bool instant: false
      property bool shrank: false
      function revealNow() { instant = true; opacity = 0.85 }
      visible: requested
      width: seat.width - 2 * seat.labelGutter
      height: seat.trayHeight
      x: seat.titleCenterX - width / 2
      // Keep waiting devices clear of the seated device icon.
      y: seat.ellipseTop - seat.tallestSeatedRise - seat.waitingGap - height
      opacity: 0
      onRequestedChanged: if (!requested) { opacity = 0; instant = false }

      SequentialAnimation {
        running: waitingStack.requested && !waitingStack.instant
        PauseAnimation { duration: waitingStack.revealDelay }
        NumberAnimation {
          target: waitingStack
          property: "opacity"
          from: 0
          to: 0.85
          duration: waitingStack.fadeDuration
          easing.type: Easing.OutCubic
        }
      }

      Repeater {
        id: waitingDevices
        model: waitingModel
        delegate: Column {
          id: waitingDevice
          required property var device
          required property int index
          readonly property string label: String(device.label || "")
          readonly property bool phone: Model.isPhone(label)
          // Painted size. The icon item stays at its unscaled width and shrinks from its bottom center.
          readonly property real iconSpan: (phone ? 20 : 35) * waitingStack.iconScale
          readonly property real labelSpan: Math.min(waitingLabel.implicitWidth, waitingStack.slotWidth)
          readonly property string key: Model.normalizeAddress(device.address || "")
          // A device already waiting when the tray opens fades in once the
          // displaced device has landed; the displaced device itself glides.
          property real entryFade: 1
          Component.onCompleted: {
            var freshDeparture = false
            for (var other in motionHistory.departed)
              if (Date.now() - motionHistory.departed[other].at < 1000) freshDeparture = true
            if (!motionHistory.departed[key] && freshDeparture) { entryFade = 0; entryFadeTimer.start() }
          }
          Timer { id: entryFadeTimer; interval: root.moveMs + 150; onTriggered: entryFadeAnim.start() }
          NumberAnimation {
            id: entryFadeAnim
            target: waitingDevice; property: "entryFade"; to: 1
            duration: 300; easing.type: Easing.OutCubic
          }
          readonly property real glideX: motion.shift.x
          readonly property real glideY: motion.shift.y
          opacity: (motion.pending ? 0 : seat.waitingUnconfirmed ? 0.45 : 1) * entryFade
          transform: motion.shift
          // Capture before removal invalidates the model index.
          function recordPlacement() { motion.capture() }
          objectName: "waiting" + seat.title + index
          width: Math.max(iconSpan, labelSpan, enrollButton.visible ? enrollButton.width : 0)
          // A lone riser that sat on the right of a pair waits on the right
          // of the tray, so it rises on its own side while its partner is
          // still seated; the pair order above keeps it there when the
          // partner joins it.
          readonly property int slotIndex: waitingModel.count === 1 && seat.lastPairOrder.length === 2
            && seat.lastPairOrder[1] === key ? 1 : index
          x: slotIndex * (waitingStack.slotWidth + seat.labelGutter) + (waitingStack.slotWidth - width) / 2
          onXChanged: motion.relayout(x)
          spacing: 6

          Item {
            width: parent.width
            height: 32.5 * waitingStack.iconScale
            DeviceIcon {
              id: waitingIcon
              anchors.horizontalCenter: parent.horizontalCenter
              anchors.bottom: parent.bottom
              scale: waitingStack.iconScale * motion.iconGrow
              transformOrigin: Item.Bottom
              phone: waitingDevice.phone
              level: seat.waitingLit || seat.waitingAlwaysActive ? 1 : 0
              lineColor: seat.lineColor
              screenFill: seat.screenFill
            }
          }
          Text {
            id: waitingLabel
            width: parent.width
            textFormat: Text.PlainText
            text: waitingDevice.label
            color: waitingIcon.strokeColor
            font.family: seat.fontFamily
            font.pixelSize: root.waitingLabelPx
            scale: motion.labelGrow
            transformOrigin: Item.Top
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
          }
          Button {
            id: enrollButton
            objectName: seat.title === "Omarchy" ? "fleetEnrollButton" : ""
            visible: seat.title === "Omarchy" && waitingDevice.device.discovered === true
              && !root.fleetWaitingForKeyring
            width: Math.min(waitingStack.slotWidth + seat.labelGutter - 4, contentItem.implicitWidth + 12)
            x: (parent.width - width) / 2
            height: root.enrollButtonHeight
            text: waitingDevice.device.fleetEnrolled === true ? "Join fleet" : "+ Add to fleet"
            hoverEnabled: true
            HoverHandler { cursorShape: Qt.PointingHandCursor }
            onClicked: root.enrollClicked(waitingDevice.device.address, waitingDevice.label, waitingDevice.device.fleetEnrolled === true)
            contentItem: Text {
              text: enrollButton.text
              color: root.foreground
              font.family: seat.fontFamily
              font.pixelSize: 8
              horizontalAlignment: Text.AlignHCenter
              verticalAlignment: Text.AlignVCenter
            }
            background: Rectangle {
              radius: 4
              color: Qt.alpha(root.accent, enrollButton.down ? 0.3 : enrollButton.hovered ? 0.23 : 0.15)
              border.color: root.accent
              border.width: enrollButton.activeFocus ? 2 : 1
            }
          }
          Glide {
            id: motion
            icon: waitingIcon
            labelItem: waitingLabel
            address: waitingDevice.key
            shouldRecenter: function () { return waitingStack.shrank }
            onRising: waitingStack.revealNow()
          }
        }
      }
    }

    Shape {
      id: pad
      objectName: "seatPad" + seat.title
      z: 0
      x: (parent.width - seat.padW) / 2
      y: seat.padY
      width: seat.padW
      height: seat.padH
      preferredRendererType: Shape.CurveRenderer

      ShapePath {
        strokeColor: "transparent"
        fillColor: seat.screenFill
        PathAngleArc {
          centerX: pad.width / 2
          centerY: pad.height / 2
          radiusX: seat.ellipseRadiusX
          radiusY: pad.height * seat.padFlat
          startAngle: 0
          sweepAngle: 360
        }
      }
      // A connected pair leaves the arc between its two connectors' inner
      // edges undrawn, with flat ends at those edges; any other seat's
      // ellipse stays closed, and its join lands flush on it instead.
      ShapePath {
        strokeColor: seat.ellipsePaint
        strokeWidth: seat.ellipseWidth
        strokeStyle: seat.connectorConnected ? ShapePath.SolidLine : ShapePath.DashLine
        dashPattern: [3, 4]
        capStyle: seat.pairGapDegrees > 0 ? ShapePath.FlatCap : ShapePath.RoundCap
        fillColor: "transparent"
        PathAngleArc {
          objectName: "seatOutline" + seat.title
          centerX: pad.width / 2
          centerY: pad.height / 2
          radiusX: seat.ellipseRadiusX
          radiusY: pad.height * seat.padFlat
          startAngle: 90 + seat.pairGapDegrees
          sweepAngle: 360 - 2 * seat.pairGapDegrees
        }
      }
    }

    // A pair gets two connectors, one per device; the glowing device's is on its side of the label.
    readonly property real pairJoinGutter: 9
    readonly property real pairJoinOffset: (linkWidth + pairJoinGutter) / 2
    readonly property real pairGapDegrees: occupantTwo && drawnConnected
      ? Math.asin(Math.min(1, pairJoinGutter / 2 / ellipseRadiusX)) * 180 / Math.PI : 0
    readonly property int joinSide: glow.litX >= 0 && glow.litX < glow.width / 2 ? -1 : 1
    // A connected seat carries its connector up to the ellipse: as dim as the ellipse in the background, lit with the audio.
    Rectangle {
      objectName: "seatJoin" + seat.title
      // Preserve the selected fractional width on both sides of the gutter.
      // Without antialiasing, pixel rounding can make one join a pixel thinner.
      antialiasing: true
      readonly property real joinTop: y
      readonly property real joinBottom: y + height
      visible: seat.connectorConnected
      z: 1
      width: seat.linkWidth
      x: (seat.width - width) / 2 + (seat.occupantTwo ? seat.joinSide * seat.pairJoinOffset : 0)
      // It lands flush on the stroke's outer edge (the glow stroke's when
      // lit): landing there shares an edge without overlapping it, while
      // running into the stroke composites a bright dot where they join.
      y: seat.linkState === "claimed" ? seat.ellipseBottom + glow.stroke / 2 : seat.ellipseBottom + seat.ellipseWidth / 2
      height: seat.titleTop - y
      color: seat.linkState === "claimed" ? seat.lineColor : seat.ellipsePaint
    }
    // The other device in a pair is connected too, so its connector is as bright as the glowing device's.
    Rectangle {
      objectName: "seatJoinSecond" + seat.title
      antialiasing: true
      visible: seat.drawnConnected && !!seat.occupantTwo
      z: 1
      width: seat.linkWidth
      x: (seat.width - width) / 2 - seat.joinSide * seat.pairJoinOffset
      y: seat.linkState === "claimed" ? seat.ellipseBottom + glow.stroke / 2 : seat.ellipseBottom + seat.ellipseWidth / 2
      height: seat.titleTop - y
      color: seat.linkState === "claimed" ? seat.lineColor : seat.ellipsePaint
    }
    // A disconnected seat ties its label to its ellipse with a dashed line, like the ellipse and its wire.
    Shape {
      objectName: "seatJoinDashed" + seat.title
      readonly property real joinTop: seat.ellipseBottom + seat.ellipseWidth / 2
      readonly property real joinBottom: seat.titleTop
      visible: !seat.connectorConnected
      z: 1
      anchors.fill: parent
      preferredRendererType: Shape.CurveRenderer
      ShapePath {
        strokeColor: seat.ellipsePaint
        strokeWidth: seat.linkWidth
        strokeStyle: ShapePath.DashLine
        dashPattern: [3, 4]
        capStyle: ShapePath.FlatCap
        fillColor: "transparent"
        startX: seat.width / 2
        startY: seat.ellipseBottom + seat.ellipseWidth / 2
        PathLine { x: seat.width / 2; y: seat.titleTop }
      }
    }

    // The ellipse glows where the connector meets it and fades along the stroke away from that point.
    Canvas {
      id: glow
      objectName: "seatGlow" + seat.title
      readonly property real margin: 2
      readonly property real stroke: 1.5
      visible: seat.linkState === "claimed"
      z: 1
      x: pad.x - margin
      y: pad.y - margin
      width: pad.width + 2 * margin
      height: pad.height + 2 * margin
      onWidthChanged: requestPaint()
      onHeightChanged: requestPaint()
      onVisibleChanged: if (visible) requestPaint()
      // In a pair, the glowing device's center, in this canvas; -1 when the seat has no glowing partner.
      readonly property var litPerson: !seat.occupantTwo ? null : secondPerson.lit ? secondPerson : firstPerson.lit ? firstPerson : null
      readonly property real litX: litPerson ? iconStack.x + litPerson.x + litPerson.iconWidth / 2 - x : -1
      onLitXChanged: requestPaint()
      Connections { target: seat; function onLineColorChanged() { glow.requestPaint() } }
      function shade(alpha) { return Qt.rgba(seat.lineColor.r, seat.lineColor.g, seat.lineColor.b, alpha) }
      // Stops run counterclockwise from a start angle, so brightness depends only on the angle from the connector.
      function falloff() {
        if (litX < 0) return { start: -Math.PI / 2,
          stops: [[0, 1], [0.1, 0.55], [0.22, 0.2], [0.38, 0], [0.62, 0], [0.78, 0.2], [0.9, 0.55], [1, 1]] }
        // The path runs from the active connector toward the glowing device and ends under it. The 0/1 seam sits on
        // that connector's far edge; the other connector's glow spreads a little up its own side, short of its device.
        const rx = seat.ellipseRadiusX, ry = pad.height * seat.padFlat
        const dx = Math.min(Math.abs(litX - width / 2), rx * 0.98)
        const edge = Math.atan2(seat.linkWidth / 2, ry)
        const join = seat.pairJoinOffset
        const joinAngle = Math.atan2(join, ry * Math.sqrt(1 - join * join / (rx * rx)))
        const turn = 2 * Math.PI
        const end = (edge + Math.atan2(dx, -ry * Math.sqrt(1 - dx * dx / (rx * rx))) - joinAngle) / turn
        // Each connector's inner edge is a hard stop, so no light bleeds into the gutter between them.
        const other = 1 - (2 * joinAngle - edge) / turn, otherInner = other + edge / turn, hard = 0.0005, spread = 0.12
        const toward = [[0, 1], [end / 2, 0.7], [end, 1], [end + 0.02, 0], [other - spread, 0], [other - spread / 2, 0.4],
          [otherInner, 1], [otherInner + hard, 0], [1, 0]]
        if (litX > width / 2) return { start: -Math.PI / 2 + joinAngle - edge, stops: toward }
        return { start: -Math.PI / 2 - joinAngle + edge, stops: toward.map(stop => [1 - stop[0], stop[1]]).reverse() }
      }
      function ring(context, lineWidth, alpha) {
        const path = falloff()
        const gradient = context.createConicalGradient(width / 2, height / 2, path.start)
        for (const stop of path.stops) gradient.addColorStop(stop[0], shade(alpha * stop[1]))
        context.strokeStyle = gradient
        context.lineWidth = lineWidth
        context.beginPath()
        context.ellipse(margin + pad.width / 2 - seat.ellipseRadiusX, margin + pad.height / 2 - pad.height * seat.padFlat,
          2 * seat.ellipseRadiusX, 2 * pad.height * seat.padFlat)
        context.stroke()
      }
      onPaint: {
        const context = getContext("2d")
        context.reset()
        ring(context, stroke, 1)
      }
    }

    Item {
      id: iconStack
      // A pair mirrors the ellipse center. Labels keep a gutter between them.
      readonly property real centerDistance: !(seat.occupantTwo || seat.pairIncoming) ? 0 : Math.max(
        seat.pairGap + firstPerson.iconWidth / 2 + secondPerson.iconWidth / 2,
        seat.labelGutter + firstPerson.labelWidth / 2 + secondPerson.labelWidth / 2)
      z: 8
      anchors.left: parent.left
      anchors.right: parent.right
      height: Math.max(firstPerson.height, secondPerson.height)
      y: seat.ellipseTop - seat.stackChipH + seat.iconOverlap

      Occupant {
        id: firstPerson
        x: seat.occupantTwo
          ? parent.width / 2 - iconStack.centerDistance / 2 - iconWidth / 2
          : seat.pairIncoming
            ? parent.width / 2 + iconStack.centerDistance / 2 - iconWidth / 2
            : (parent.width - iconWidth) / 2
        channel: seat
        visible: !!seat.occupant && !seat.occupant.standIn
        label: seat.occupantLabel
        phone: seat.occupantPhone
        onAcl: seat.occupantOnAcl
        deviceIndex: 0
        address: seat.occupant ? seat.occupant.address || "" : ""
      }
      Occupant {
        id: secondPerson
        x: parent.width / 2 + iconStack.centerDistance / 2 - iconWidth / 2
        channel: seat
        visible: !!seat.occupantTwo && !seat.occupantTwo.standIn
        // While a pair is incoming, the phantom slot carries the arriving
        // device's label so the pair spacing already matches the formed pair.
        label: seat.occupantTwo ? seat.occupantTwoLabel : seat.incomingLabel
        phone: seat.occupantTwoPhone
        onAcl: seat.occupantTwoOnAcl
        deviceIndex: 1
        address: seat.occupantTwo ? seat.occupantTwo.address || "" : ""
      }
    }

    Text {
      objectName: "seatAction" + seat.title
      visible: !seat.occupant && text !== ""
      z: 8
      anchors.horizontalCenter: parent.horizontalCenter
      y: seat.ellipseCenter - height / 2
      text: seat.actionLabel
      // An in-progress label ("Reconnecting…") draws like the status label
      // ("Waiting…"): the seat colour at ellipse opacity. A clickable
      // action ("Reconnect") stays full brightness.
      color: seat.lineColor
      opacity: seat.actionEnabled ? 1 : seat.ellipseOpacity
      font.family: seat.fontFamily
      font.pixelSize: 12
    }

    // A connection status is dim and unclickable while the seat is empty.
    Text {
      objectName: "seatStatus" + seat.title
      z: 8
      anchors.horizontalCenter: parent.horizontalCenter
      y: seat.ellipseCenter - height / 2
      text: seat.statusText
      // Drawn like the dashed ellipse around it, so it reads as part of the seat.
      color: seat.lineColor
      font.family: seat.fontFamily
      font.pixelSize: 12
      horizontalAlignment: Text.AlignHCenter
      property real shown: seat.statusText && !seat.occupant ? 1 : 0
      Behavior on shown { NumberAnimation { duration: 200; easing.type: Easing.OutSine } }
      opacity: shown * seat.ellipseOpacity
      visible: shown > 0
    }

    // Each seat holds one member of a fleet: Apple's devices or the Omarchy laptops.
    Column {
      id: seatTitle
      anchors.horizontalCenter: parent.horizontalCenter
      y: root.seatsHeight

      Text {
        anchors.horizontalCenter: parent.horizontalCenter
        text: seat.title
        color: seat.ink
        font.family: seat.fontFamily
        font.pixelSize: 13
        horizontalAlignment: Text.AlignHCenter
      }

      Text {
        objectName: seat.objectName + "FleetLabel"
        anchors.horizontalCenter: parent.horizontalCenter
        text: "FLEET"
        color: seat.dimColor
        font.family: seat.fontFamily
        font.pixelSize: 8
        font.letterSpacing: 1.5
        horizontalAlignment: Text.AlignHCenter
      }
    }

    MouseArea {
      objectName: "seatClick" + seat.title
      z: 10
      anchors.fill: parent
      enabled: !seat.clickSuspended && (!seat.actionLabel || seat.actionEnabled)
      preventStealing: true
      cursorShape: Qt.PointingHandCursor
      onClicked: seat.clicked()
    }
  }

  // Carries an icon from where it was last drawn to its slot, growing or shrinking as it crosses between a seat
  // and the tray.
  component Glide: Item {
    id: motion
    // Never drawn, so a Column holding it does not space it as a row.
    visible: false
    readonly property alias shift: offset
    property real iconGrow: 1
    property real labelGrow: 1
    property int holdMs: 0
    property bool leavingSeat: false
    property bool slidingLeftInTray: false
    readonly property int easing: leavingSeat ? Easing.InCubic : Easing.OutCubic
    readonly property bool running: settle.running || horizontal.running
    required property Item icon
    required property Item labelItem
    property string address: ""
    property bool seated: false
    // Read layout state inside its change handler, before dependent bindings catch up.
    required property var shouldRecenter
    property bool pending: !seated
    readonly property bool arriving: pending || (running && fromTray)
    property bool fromTray: false
    property string lastAddress: ""
    property var slotFrom: null
    property real restX: NaN
    property string restAddress: ""
    property bool laidOut: false
    signal rising

    function paintedPoint() { return icon.mapToItem(root, icon.width / 2, icon.height) }
    function destination() {
      const point = paintedPoint()
      return Qt.point(point.x - offset.x, point.y - offset.y)
    }
    function capture() {
      motionHistory.capture(address, paintedPoint(), seated, icon.scale, labelItem.font.pixelSize * labelItem.scale)
    }
    function recordRest() {
      if (seated && address && !running) motionHistory.rest(address, paintedPoint())
    }
    function arrive() {
      pending = false
      const from = motionHistory.origin(address, seated, slotFrom)
      slotFrom = null
      if (!address || !from) return
      const here = destination()
      if (Math.abs(from.x - here.x) < 1 && Math.abs(from.y - here.y) < 1) return
      leavingSeat = !seated && from.seated
      slidingLeftInTray = !seated && !from.seated && from.x > here.x
      holdMs = from.seated === seated && shouldRecenter() ? root.recenterDelayMs : 0
      fromTray = seated && !from.seated
      if (leavingSeat) {
        rising()
        iconGrow = 1 / root.waitingIconScale
        labelGrow = root.seatedLabelPx / root.waitingLabelPx
      } else if (fromTray) {
        iconGrow = from.iconScale
        labelGrow = from.labelSize / root.seatedLabelPx
      }
      start(from.x - here.x, from.y - here.y)
    }
    function relayout(x) {
      if (seated) {
        restRecord.restart()
        if (laidOut && !isNaN(restX) && address && address === restAddress && !running && Math.abs(x - restX) > 1) {
          holdMs = shouldRecenter() ? root.recenterDelayMs : 0
          start(offset.x + restX - x, offset.y)
        }
      } else if (!pending && !isNaN(restX) && Math.abs(x - restX) > 1) {
        holdMs = !running && shouldRecenter() ? root.traySwapStaggerMs : 0
        slidingLeftInTray = x < restX
        shiftX(restX - x)
      }
      restX = x
      restAddress = address
    }
    onAddressChanged: {
      if (!seated) return
      if (lastAddress) motionHistory.departure(lastAddress, destination(), icon.scale, labelItem.font.pixelSize * labelItem.scale, labelItem.text)
      lastAddress = address
      // Snapshot the old seat before layout; a removed tray icon can supersede it on the next tick.
      slotFrom = address ? motionHistory.seatedOrigin(address) : null
      restRecord.restart()
      if (address) { pending = true; glideStart.restart() }
    }
    Timer { id: glideStart; interval: 0; onTriggered: motion.arrive() }
    Timer { id: restRecord; interval: 0; onTriggered: motion.recordRest() }
    Timer { id: layoutDone; interval: 0; onTriggered: { motion.laidOut = true; motion.recordRest() } }
    Component.onCompleted: {
      if (seated) layoutDone.start()
      else glideStart.start()
    }
    function start(dx, dy) {
      offset.x = dx
      offset.y = dy
      horizontal.restart()
      settle.restart()
    }
    function shiftX(dx) {
      offset.x += dx
      horizontal.restart()
    }
    Translate { id: offset }
    SequentialAnimation {
      id: horizontal
      onFinished: if (!settle.running) motion.recordRest()
      PauseAnimation { duration: motion.holdMs }
      NumberAnimation {
        target: offset; property: "x"; to: 0
        duration: motion.slidingLeftInTray ? root.traySwapMoveMs
          : motion.leavingSeat ? root.riseHorizontalMs : root.moveMs
        easing: motion.slidingLeftInTray ? { type: Easing.InOutCirc }
          : motion.leavingSeat ? { type: Easing.BezierSpline, bezierCurve: root.riseHorizontalCurve }
          : { type: Easing.OutCubic }
      }
    }
    SequentialAnimation {
      id: settle
      onFinished: if (!horizontal.running) motion.recordRest()
      PauseAnimation { duration: motion.holdMs }
      ParallelAnimation {
        NumberAnimation { target: offset; property: "y"; to: 0; duration: root.moveMs; easing.type: motion.easing }
        NumberAnimation { target: motion; property: "iconGrow"; to: 1; duration: root.moveMs; easing.type: motion.easing }
        NumberAnimation { target: motion; property: "labelGrow"; to: 1; duration: root.moveMs; easing.type: motion.easing }
      }
    }
  }

  component Occupant: Item {
    id: person
    required property SeatChannel channel
    property string label
    property bool phone
    property bool onAcl
    property int deviceIndex
    property string address
    readonly property real iconWidth: device.width
    // The ellipse glow follows the audio holder only; a seated device can
    // draw brighter than that (below) without claiming the glow.
    readonly property bool lit: Model.seatDeviceLit({ address: person.address, onAcl: person.onAcl },
      person.channel.layout, person.channel.ownsAudio)
    readonly property real labelWidth: name.implicitWidth
    readonly property real labelGap: person.channel.padH * person.channel.padFlat
      - person.channel.iconOverlap - name.implicitHeight / 2
    // A device leaving the seat records where it sat, so its waiting icon rises from there.
    // One arriving glides from where it last waited.
    readonly property string shown: visible ? Model.normalizeAddress(address) : ""
    readonly property real glideX: motion.shift.x
    readonly property real glideY: motion.shift.y
    opacity: motion.pending ? 0 : 1
    readonly property bool arriving: motion.arriving
    onXChanged: motion.relayout(x)
    transform: motion.shift
    Glide {
      id: motion
      icon: device
      labelItem: name
      address: person.shown
      seated: true
      shouldRecenter: function () { return !person.channel.occupantTwo && !person.channel.pairIncoming }
    }
    width: iconWidth
    height: iconSlot.height + labelGap + name.implicitHeight

    Item {
      id: iconSlot
      width: person.iconWidth
      height: person.channel.stackChipH
      DeviceIcon {
        id: device
        objectName: "device" + person.channel.title + person.deviceIndex
        // Brightness tiers: off the link dim, seated without ownership
        // active, seated with ownership full.
        level: !person.onAcl ? 0
          : Model.seatDeviceLit({ address: person.address, onAcl: person.onAcl }, person.channel.layout,
            person.channel.ownsAudio) ? 2 : 1
        playing: root.audioPlaying
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        scale: motion.iconGrow
        transformOrigin: Item.Bottom
        phone: person.phone
        lineColor: person.onAcl ? person.channel.ink : person.channel.dimColor
        screenFill: person.channel.screenFill
      }
    }
    Text {
      id: name
      objectName: "name" + person.channel.title + person.deviceIndex
      anchors.horizontalCenter: iconSlot.horizontalCenter
      anchors.top: iconSlot.bottom
      anchors.topMargin: person.labelGap
      text: person.label
      color: person.onAcl ? person.channel.ink : person.channel.dimColor
      // Label shade follows the icon tier.
      opacity: person.onAcl ? (device.level >= 2 ? 1 : device.level === 1 ? 0.75 : 0.5) : 1
      font.family: person.channel.fontFamily
      font.pixelSize: root.seatedLabelPx
      scale: motion.labelGrow
      transformOrigin: Item.Top
    }
  }

}
