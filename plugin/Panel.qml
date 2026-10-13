import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "io.github.danmacdonald.fleetpods"
  ipcTarget: "fleetpods"
  manageIpc: false

  property string shownFleetId: ""
  property string dismissedFleetId: "-"
  property bool fleetAutoOpened: false
  property var fleetChoicePeer: null
  readonly property bool fleetVisible: fleetChoicePeer !== null || (pods.view.fleet.phase !== "idle" && !!pods.view.fleet.phase
    && String(pods.view.fleet.id || "") !== dismissedFleetId)

  function chooseFleet(address, name, peerEnrolled) {
    if (pods.fleetBusy) return
    if (peerEnrolled && pods.view.fleet.enrolled === true) {
      fleetChoice.stage = "leave"
      fleetChoice.error = ""
      fleetChoicePeer = { address: address, name: name }
    } else {
      dismissedFleetId = "-"
      pods.fleetCommand({ action: peerEnrolled ? "join" : "invite", address: address, name: name })
    }
  }
  function dismissFleetChoice() {
    if (pods.fleetBusy) return
    var invitationId = fleetChoicePeer ? fleetChoicePeer.invitationId : ""
    if (invitationId) dismissedFleetId = invitationId
    fleetChoicePeer = null
    if (invitationId) pods.fleetCommand({ action: "cancel", id: invitationId })
  }
  function sendFleetChoice(join) {
    if (!fleetChoicePeer || pods.fleetBusy) return
    if (join && fleetChoicePeer.invitationId) {
      pods.fleetCommand({ action: "leave_invitation", id: fleetChoicePeer.invitationId })
      return
    }
    pods.fleetCommand({ action: join ? "join" : "invite", address: fleetChoicePeer.address,
      name: fleetChoicePeer.name, leave: join })
  }
  Connections {
    target: pods
    function onFleetRequestFinished(ok, error) {
      if (!root.fleetChoicePeer) return
      if (ok) { root.fleetChoicePeer = null; root.dismissedFleetId = "-" }
      else fleetChoice.error = error
    }
  }

  function syncFleet() {
    var state = pods.view.fleet
    if ((state.phase === "incoming" || state.phase === "inviting") && state.id !== shownFleetId) {
      shownFleetId = state.id
      dismissedFleetId = "-"
      fleetAutoOpened = !opened
      root.open()
      var peerAddress = Model.normalizeAddress(state.peer_address)
      var known = pods.view.devices[peerAddress]
      if (state.phase === "incoming" && state.enrolled === true && !(known && known.discovered !== true)) {
        fleetChoice.stage = "leave"
        fleetChoice.error = ""
        root.fleetChoicePeer = { address: peerAddress, name: state.peer_name, invitationId: state.id }
      }
      Qt.callLater(function() { if (!root.fleetChoicePeer) fleetPane.codeInput.forceActiveFocus() })
    }
    if (state.phase === "cancelled" || state.phase === "expired") {
      dismissedFleetId = String(state.id || "")
      if (fleetAutoOpened) root.close()
      fleetAutoOpened = false
    }
  }

  property int cursorIndex: 0
  property bool cursorActive: false

  readonly property bool hideWhenDisconnected: setting("hideWhenDisconnected", true) === true
  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground
  readonly property color urgent: bar ? bar.urgent : Commons.Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property color barIconColor: root.barForeground
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  // Say nothing extra once a section already explains the state.
  readonly property bool guidanceVisible: pods.statusLogState.guidanceVisible === true && !pods.schemaUnsupported
  readonly property bool pairingRequired: pods.statusLogState.pairingRequired === true

  property int phraseIndex: 0

  readonly property int lowBatteryPercent: 20
  readonly property int adaptiveStepPercent: 5
  readonly property int phraseIntervalMs: 2800

  // Ten, matching the stock panels.
  readonly property var activePhrases: [
    "All gone Pete Tong",
    "Essential Selection",
    "Genius Bar stumped",
    "Warming the decks",
    "Hands in the air",
    "Ibiza incoming",
    "Rinsing the low end",
    "Big tune loading",
    "Cued and counting in",
    "Ecosystem escaped"
  ]
  readonly property string heroPhraseText: activePhrases[phraseIndex % activePhrases.length]

  readonly property bool adaptiveVisible: pods.view.hasAirPods && pods.view.supportsAdaptive && pods.noiseMode === Model.NOISE_ADAPTIVE
  // Three separate capabilities, because a Max 2 has Conversation Awareness and no second bud.
  readonly property bool modesVisible: pods.view.hasAirPods && modes.length > 0
  readonly property bool caVisible: pods.view.hasAirPods && pods.view.supportsConversationalAwareness
  readonly property bool oneBudVisible: pods.view.hasAirPods && pods.view.supportsOneBudANC
  readonly property string lidLabel: Model.lidText(pods.view.lidState)
  readonly property bool podsInCase: {
    var _ = nearbyTick
    return !Model.budsOut(pods.view.lidState, pods.view.leftPod, pods.view.rightPod, pods.view.lastAdvertisementMs, Date.now(), pods.view.connected, 20000, pods.view.caseBattery)
  }
  readonly property bool showConnecting: (pods.view.connecting || pods.view.holdingOmarchySeat || root.takingOmarchySeat) && !podsInCase && (pods.view.hasAirPods || pods.view.hasBattery || pods.view.holdingOmarchySeat || root.takingOmarchySeat)
  // Cold Bluetooth setup is about 13s on this box; the ring is not a measured percentage.
  readonly property int connectionSpinMs: {
    var value = Number(setting("connectionSpinMs", 13000))
    return value >= 3000 && value <= 20000 ? value : 13000
  }
  readonly property int connectionCompleteFillMs: 200
  readonly property int connectionCompleteHoldMs: 500
  readonly property int connectionCompleteFadeMs: 200
  // 0 hidden, 1 spinning arc, 2 fill from the captured sweep then hold then fade.
  // Connect end swaps canvases in one frame so the arc is never rewritten in place.
  property int connectionRingState: 0
  property real connectionSweep: 0.15
  property real connectionDoneSweep: 0.15
  property real connectionDoneOpacity: 0
  property bool connectionSpinVisible: false

  function syncConnectionRing() {
    if (pods.view.connecting) {
      if (connectionRingState !== 1) connectionRingState = 1
      return
    }
    if (connectionRingState === 1) connectionRingState = 2
  }

  // The daemon names the family, so the mark is the shape of the device actually connected.
  readonly property string podsVariant: Model.iconVariant(pods.view.isHeadset, pods.view.isProSeries, pods.view.modelInt)
  readonly property string barCaseState: pods.caseIconState
  readonly property bool connectionElsewhere: {
    var _ = nearbyTick
    return pods.view.daemonReachable && Model.connectionElsewhere(pods.view.connected,
      pods.view.bleConnectionState, pods.view.lastAdvertisementMs, Date.now(), pods.view.disconnectedMs,
      pods.view.bluezConnected)
  }

  // The daemon decides which modes this device has, so no row is drawn that the hardware ignores.
  readonly property var modes: pods.availableModes()

  // Rebuilt whenever a section appears, so j and k never land on a hidden control.
  property int nearbyTick: 0
  readonly property bool telemetryUnavailable: {
    var _ = nearbyTick
    return Model.telemetryUnavailable(pods.view.connected, pods.view.lastAdvertisementMs, Date.now())
  }
  // A QML int is 32 bits and wraps a wall-clock millisecond, which kept the last
  // charge on screen after the case had already gone quiet.
  readonly property real batteryNow: {
    var _ = nearbyTick
    return Date.now()
  }
  readonly property string domainDescription: pods.statusLogState.description || ""
  readonly property string domainSubtitle: pods.statusLogState.subtitle || ""

  StatusHistory {
    id: statusHistory
    rows: pods.statusLogState.rows
  }
  readonly property bool podsNearby: {
    var _ = nearbyTick
    return Model.podsNearby(pods.view.lastAdvertisementMs, pods.view.lidState, Date.now())
  }
  // The status log computes the seats in the same step as the rest of the panel's status, so they never half-apply.
  readonly property var seatPresentation: pods.statusLogState.seat
  readonly property var fleetView: root.seatPresentation.fleetView
  // Diagnostics only: the header showed this line too briefly to read, and it made the panel jump.
  readonly property string localConnectionLabel: pods.view.daemonReachable ? Model.localConnectionLabel(pods.view) : ""
  readonly property var displayedSeats: root.seatPresentation.seats
  readonly property bool ownershipUnknown: root.seatPresentation.ownershipUnknown
  // A confirmed Apple owner wins over an in-progress Omarchy click. Otherwise
  // the claim flag keeps Omarchy in front after the iPhone has already taken the buds.
  readonly property bool appleOwnsAudio: Model.appleOwnsAudio(pods.view)
  readonly property bool takingOmarchySeat: pods.statusLogState.taking === true
  readonly property bool linuxHeld: pods.statusLogState.linuxHeld === true
  function beginOmarchyClaim(origin) {
    if (root.pairingRequired) return
    console.log("fleetpods ownership requested via " + origin)
    pods.noteSeatClick()
    pods.claimOwnership()
  }
  property string selectedSeat: linuxHeld || pods.view.remoteOmarchyAudio ? "omarchy" : "apple"

  readonly property bool showSeats: pods.statusLogState.showSeats === true
  readonly property var cursorRows: root.showSeats && !root.pairingRequired ? ["apple", "omarchy"] : []

  readonly property string cursorRow: cursorRows.length === 0
    ? ""
    : cursorRows[Math.max(0, Math.min(cursorIndex, cursorRows.length - 1))]

  function rowHasCursor(name) {
    return cursorActive && cursorRow === name
  }

  function moveCursor(dy) {
    cursorActive = true
    if (cursorRows.length === 0) return
    cursorIndex = Math.max(0, Math.min(cursorRows.length - 1, cursorIndex + dy))
  }

  function nudgeCursor(dx) {
    if (dx === 0 || cursorRows.length === 0) return
    cursorActive = true
    cursorIndex = dx < 0 ? 0 : 1
    selectedSeat = cursorRow
  }

  function activateCursor() {
    if (cursorRow === "apple") seats.activateApple()
    else if (cursorRow === "omarchy") beginOmarchyClaim("keyboard Enter")
  }

  function focusRow(name) {
    var at = cursorRows.indexOf(name)
    if (at < 0) return
    cursorActive = true
    cursorIndex = at
  }

  visible: true
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight
  Component.onCompleted: {
    console.log("fleetpods-ring panel-ready fade_ms=" + connectionCompleteFadeMs
      + " activity_hold_ms=" + pods.activityHoldMs + " keys=arrows-enter activity=fresh-input audio=domain roster=fleet local-link=explicit caption=local")
  }

  onOpenedChanged: if (opened) {
    statusConsole.expanded = false
    cursorActive = false
    cursorIndex = 0
    pods.refresh()
    if (!fleetVisible) pods.requestActivity("panel-opened")
    Qt.callLater(function () {
      if (fleetVisible && pods.view.fleet.phase === "incoming") fleetPane.codeInput.forceActiveFocus()
      else if (fleetVisible) fleetOverlay.forceActiveFocus()
      else keyCatcher.forceActiveFocus()
    })
  }

  Service {
    id: pods
    settings: root.settings
    onViewChanged: root.syncFleet()
  }
  readonly property bool podsConnecting: pods.view.connecting === true
  onPodsConnectingChanged: syncConnectionRing()

  Timer {
    interval: 2000
    running: true
    repeat: true
    onTriggered: { pods.refresh(); pods.tickCaseIcon(); root.nearbyTick++ }
  }

  IpcHandler {
    target: root.ipcTarget
    function open(): void { root.open() }
    function close(): void { root.close() }
    function toggle(): void { root.toggle() }
    function refresh(): string { pods.refresh(); return "ok" }
    function noise(): string { pods.cycleNoiseMode(); return "ok" }
    function status(): string { return Model.noiseModeName(pods.noiseMode) }
    function connecting(): bool { return root.showConnecting }
    function iconState(): string {
      return JSON.stringify({ connected: pods.view.hasAirPods, audioHere: pods.view.audioHere,
        remoteOmarchyAudio: pods.view.remoteOmarchyAudio, omarchyOwnsAudio: seats.omarchyOwnsAudio,
        appleOwnsAudio: seats.appleOwnsAudio, subtitle: root.domainSubtitle,
        fleetRoster: !!root.fleetView, localConnection: root.localConnectionLabel,
        seatedOmarchy: root.displayedSeats.omarchy.map(function (device) { return device.label }),
        seatedApple: root.displayedSeats.apple.map(function (device) { return device.label }),
        omarchyPrompt: root.seatPresentation.omarchyTransitionLabel,
        appleSeatUnknown: root.seatPresentation.appleSeatUnknown === true,
        waitingOmarchy: seats.availableOmarchy.map(function (device) { return device.label }),
        caseState: root.barCaseState, reportedLid: pods.view.lidState,
        pairingMode: pods.view.pairingMode, batteriesVisible: seats.showBattery,
        panelAvailable: true, panelOpen: root.opened, seatsVisible: root.showSeats,
        diagram: { visible: seats.visible, height: seats.height, contentHeight: panelContent.height,
          scale: column.scale, connectorPhase: seats.connectorPhase, connectorProgress: seats.connectionProgress },
        telemetryUnavailable: root.telemetryUnavailable, ownershipUnknown: root.ownershipUnknown, description: root.domainDescription,
        color: String(root.barIconColor), foreground: String(root.barForeground) })
    }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component {
      Item {
        AirPodsIcon {
          anchors.centerIn: parent
          // A pair of buds is wider than it is tall, so it takes a size above the stock 12 to carry the row.
          iconSize: Style.space(13)
          color: root.barIconColor
          variant: root.podsVariant
          caseState: root.barCaseState
        }
      }
    }
    onPressed: function (buttonCode) {
      if (buttonCode === Qt.RightButton) pods.cycleNoiseMode()
      else root.toggle()
    }
  }

  Item {
    id: connectionMark
    anchors.centerIn: button
    width: Style.space(21)
    height: width
    z: 1

    function applyState() {
      if (root.connectionRingState === 1) {
        completion.stop()
        root.connectionDoneOpacity = 0
        root.connectionDoneSweep = 0.15
        root.connectionSpinVisible = true
        spinning.restart()
        return
      }
      if (root.connectionRingState === 2) {
        spinning.stop()
        completion.stop()
        var captured = root.connectionSweep
        fill.from = captured
        root.connectionDoneSweep = captured
        root.connectionDoneOpacity = 1
        root.connectionSpinVisible = false
        completion.restart()
        return
      }
      spinning.stop()
      completion.stop()
      root.connectionSpinVisible = false
      root.connectionDoneOpacity = 0
    }

    Component.onCompleted: applyState()
    Connections {
      target: root
      function onConnectionRingStateChanged() { connectionMark.applyState() }
    }

    ConnectionRing {
      anchors.fill: parent
      ringColor: root.barForeground
      spinning: root.connectionSpinVisible
      sweep: root.connectionSweep
      doneSweep: root.connectionDoneSweep
      doneOpacity: root.connectionDoneOpacity
    }

    NumberAnimation {
      id: spinning
      target: root
      property: "connectionSweep"
      from: 0.15
      to: Math.PI * 2
      duration: root.connectionSpinMs
      loops: Animation.Infinite
      alwaysRunToEnd: false
    }

    SequentialAnimation {
      id: completion
      NumberAnimation {
        id: fill
        target: root
        property: "connectionDoneSweep"
        to: Math.PI * 2
        duration: root.connectionCompleteFillMs
      }
      PauseAnimation {
        duration: root.connectionCompleteHoldMs
      }
      NumberAnimation {
        target: root
        property: "connectionDoneOpacity"
        to: 0
        duration: root.connectionCompleteFadeMs
      }
      onFinished: if (root.connectionRingState === 2) root.connectionRingState = 0
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: root.fleetVisible ? (fleetPane.incoming ? fleetPane.codeInput : fleetOverlay) : keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(440))
    contentHeight: panel.fittedContentHeight(Math.max(
      column.implicitHeight + statusConsole.height + (statusConsole.visible ? Style.space(12) : 0),
      root.fleetVisible ? fleetPane.implicitHeight + Style.space(64) : 0))

    SeatKeyCatcher {
      id: keyCatcher
      blocked: root.fleetVisible
      anchors.fill: parent
      onMoveRequested: function (dx, dy) {
        if (!root.cursorActive) { root.cursorActive = true; return }
        if (dy !== 0) root.moveCursor(dy)
        else if (dx !== 0) root.nudgeCursor(dx)
      }
      onActivateRequested: if (root.cursorActive) root.activateCursor()
      onCloseRequested: root.close()
      onTabRequested: function (direction) { root.switchPanel(direction) }

      Item {
        id: panelContent
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: statusConsole.top
        anchors.bottomMargin: statusConsole.visible ? Style.space(12) : 0
        clip: true

        Column {
          id: column
          width: panelContent.width
          scale: implicitHeight > 0 ? Math.min(1, panelContent.height / implicitHeight) : 1
          transformOrigin: Item.Top
          spacing: Style.space(12)

          Item {
            width: parent.width
            implicitHeight: Math.max(headerIcon.implicitHeight, headerLabels.implicitHeight)

            AirPodsIcon {
              id: headerIcon
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
              iconSize: Style.font.display
              color: root.foreground
              variant: root.podsVariant
              caseState: root.barCaseState
            }

            Column {
              id: headerLabels
              anchors.left: headerIcon.right
              anchors.leftMargin: Style.space(14)
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.space(2)

              Text {
                textFormat: Text.PlainText
                width: parent.width
                text: pods.view.modelName !== "" ? pods.view.modelName : (pods.view.deviceName !== "" ? pods.view.deviceName : "FleetPods")
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.title
                font.bold: true
                elide: Text.ElideRight
              }

              Text {
                textFormat: Text.PlainText
                visible: text !== ""
                width: parent.width
                text: root.domainSubtitle.toUpperCase()
                color: Qt.darker(root.foreground, 1.4)
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                font.letterSpacing: 1.2
                elide: Text.ElideRight
              }

            }
          }

          PanelSeparator {
            foreground: root.foreground
          }

          Text {
            visible: pods.fleetRequestError !== ""
            width: parent.width
            text: pods.fleetRequestError
            textFormat: Text.PlainText
            color: root.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            wrapMode: Text.WordWrap
          }

          SeatsView {
            id: seats
            enabled: !root.pairingRequired
            visible: root.showSeats
            width: parent.width
            height: implicitHeight
            captionGap: Style.space(12)
            appleDevices: root.displayedSeats.apple
            omarchyDevices: root.displayedSeats.omarchy
            availableApple: root.seatPresentation.availableApple
            availableOmarchy: root.seatPresentation.availableOmarchy
            linuxHeld: root.seatPresentation.linuxHeld
            ownershipUnknown: root.seatPresentation.viewOwnershipUnknown
            connectedToOmarchy: root.seatPresentation.connectedToOmarchy
            // The pulse runs only while the panel is open.
            audioPlaying: root.opened && root.seatPresentation.audioPlaying
            omarchyTransitionLabel: root.seatPresentation.omarchyTransitionLabel
            appleTrayAvailable: root.seatPresentation.appleTrayAvailable
            appleSeatUnknown: root.seatPresentation.appleSeatUnknown === true
            appleTrayRemembered: root.seatPresentation.appleTrayRemembered === true
            appleOwnsAudio: root.seatPresentation.appleOwnsAudio
            omarchyOwnsAudio: root.seatPresentation.omarchyOwnsAudio
            activeDeviceAddress: root.seatPresentation.audioDeviceAddress
            accent: Commons.Color.accent
            foreground: root.foreground
            dim: root.dim
            screenFill: Commons.Color.popups.background
            fontFamily: root.fontFamily
            podsVariant: root.podsVariant
            connectionSpinVisible: root.connectionSpinVisible
            seatSwitching: root.seatPresentation.takingOverOmarchy === true
            connectionComplete: Model.localSeatConnected(pods.view)
            caseState: root.barCaseState
            pairingMode: pods.view.pairingMode && (!pods.view.bluetoothPaired || !pods.view.pairingKeysReady)
            leftBatteryLevel: (!pods.view.bluetoothPaired || !pods.view.pairingKeysReady) ? Model.LEVEL_UNKNOWN : Model.displayedBatteryLevel(pods.view.leftPod.level, pods.view.daemonReachable, pods.view.connected, pods.view.lastAdvertisementMs, root.batteryNow)
            rightBatteryLevel: (!pods.view.bluetoothPaired || !pods.view.pairingKeysReady) ? Model.LEVEL_UNKNOWN : Model.displayedBatteryLevel(pods.view.rightPod.level, pods.view.daemonReachable, pods.view.connected, pods.view.lastAdvertisementMs, root.batteryNow)
            caseBatteryLevel: (!pods.view.bluetoothPaired || !pods.view.pairingKeysReady) ? Model.LEVEL_UNKNOWN : Model.displayedBatteryLevel(pods.view.caseBattery.level, pods.view.daemonReachable, pods.view.connected, pods.view.lastAdvertisementMs, root.batteryNow)
            headsetBatteryLevel: (!pods.view.bluetoothPaired || !pods.view.pairingKeysReady) ? Model.LEVEL_UNKNOWN : Model.displayedBatteryLevel(pods.view.headsetBattery.level, pods.view.daemonReachable, pods.view.connected, pods.view.lastAdvertisementMs, root.batteryNow)
            lowBatteryColor: root.urgent
            reconnectBusy: pods.view.controlConnection && pods.view.controlConnection.peer_reconnect === true
            reconnectEnabled: !pods.busy && !root.connectionSpinVisible && pods.view.bluetoothBonded !== false
            onEnrollClicked: function(address, name, peerEnrolled) { root.chooseFleet(address, name, peerEnrolled) }
            onReconnectClicked: pods.reconnectPeer()
            onAppleClicked: { root.selectedSeat = "apple"; pods.releaseOwnership() }
            onOmarchyClicked: root.beginOmarchyClaim("seat click")
          }

        }
      }

      StatusConsole {
        id: statusConsole
        // Guidance replaces the console without shrinking its collapsed space.
        guidanceText: Model.pairingWarning(pods.view) || (root.guidanceVisible
          ? Model.emptyGuidance(pods.view.daemonReachable, pods.view.bluetoothPaired && pods.view.pairingKeysReady,
              pods.view.modelInt, Model.podsNearby(pods.view.lastAdvertisementMs, Model.LID_UNKNOWN, root.batteryNow),
              pods.view.pairingMode, pods.view.pairingCandidates,
              pods.view.lidState === Model.LID_OPEN && pods.view.caseConfirmedDocked) : "")
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.leftMargin: -panel.padding
        anchors.rightMargin: -panel.padding
        anchors.bottom: parent.bottom
        height: visible ? implicitHeight : 0
        messages: statusHistory.entries
        playbackSummary: root.seatPresentation.audioPeer.label
          + (root.seatPresentation.audioPlaying ? " · Playing" : "")
        foreground: root.foreground
        urgent: root.urgent
        fontFamily: root.fontFamily
        deviceNames: Model.logDeviceNames(pods.view.devices, pods.view.localName)
        nameColor: Commons.Color.accent
      }

      FocusScope {
        id: fleetOverlay
        anchors.fill: parent
        visible: root.fleetVisible
        z: 10
        onVisibleChanged: {
          if (visible) {
            if (!root.fleetChoicePeer && fleetPane.incoming) fleetPane.codeInput.forceActiveFocus()
            else forceActiveFocus()
          } else if (root.opened) keyCatcher.forceActiveFocus()
        }
        Keys.onEscapePressed: {
          if (root.fleetChoicePeer) root.dismissFleetChoice()
          else fleetPane.closeDialog()
        }

        Rectangle {
          anchors.fill: parent
          anchors.margins: -panel.padding
          color: "#88000000"
          MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.AllButtons
            onWheel: function(event) { event.accepted = true }
          }
        }

        Rectangle {
          id: fleetCard
          anchors.centerIn: parent
          width: parent.width - Style.space(24)
          height: Math.min(parent.height - Style.space(24), (root.fleetChoicePeer ? fleetChoice.implicitHeight : fleetPane.implicitHeight) + 2 * padding)
          readonly property int padding: Style.space(18)
          color: Commons.Color.popups.background
          border.color: Commons.Color.accent
          border.width: 1

          Flickable {
            anchors.fill: parent
            anchors.margins: fleetCard.padding
            contentWidth: width
            contentHeight: root.fleetChoicePeer ? fleetChoice.implicitHeight : fleetPane.implicitHeight
            clip: true
            boundsBehavior: Flickable.StopAtBounds
            interactive: contentHeight > height

            FleetChoice {
              id: fleetChoice
              visible: root.fleetChoicePeer !== null
              width: parent.width
              peerName: root.fleetChoicePeer ? root.fleetChoicePeer.name : ""
              incoming: !!(root.fleetChoicePeer && root.fleetChoicePeer.invitationId)
              foreground: root.foreground
              background: Commons.Color.popups.background
              urgent: root.urgent
              fontFamily: root.fontFamily
              busy: pods.fleetBusy
              onLeaveAndJoin: root.sendFleetChoice(true)
              onInviteInstead: root.sendFleetChoice(false)
              onDismiss: root.dismissFleetChoice()
            }
            FleetPairing {
              id: fleetPane
              visible: root.fleetChoicePeer === null
              width: parent.width
              pairing: pods.view.fleet
              foreground: root.foreground
              background: Commons.Color.popups.background
              accent: Commons.Color.accent
              urgent: root.urgent
              codeFontFamily: root.fontFamily
              busy: pods.fleetBusy
              onSubmit: function(code) { pods.fleetCommand({ action: "submit", id: pods.view.fleet.id, code: code }) }
              onCancel: pods.fleetCommand({ action: "cancel", id: pods.view.fleet.id })
              onDismiss: { root.dismissedFleetId = String(pods.view.fleet.id || ""); root.fleetAutoOpened = false }
            }
          }
        }
      }
    }
  }

  component PodRow: Item {
    id: podRow
    property string label: ""
    property var pod: Model.defaultPod()
    property string meta: ""

    readonly property string metaText: meta !== "" ? meta : Model.podMeta(pod)
    readonly property bool low: pod.level !== Model.LEVEL_UNKNOWN
      && pod.level <= root.lowBatteryPercent && !pod.charging

    implicitHeight: podLayout.implicitHeight

    RowLayout {
      id: podLayout
      anchors.left: parent.left
      anchors.right: parent.right
      spacing: Style.space(8)

      Text {
        textFormat: Text.PlainText
        text: podRow.label
        color: root.foreground
        opacity: 0.6
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        // Style.space(44) was cut for Left, Right and Case, so a longer label takes the room it needs.
        Layout.preferredWidth: Math.max(Style.space(44), implicitWidth + Style.space(10))
      }

      Rectangle {
        id: meterTrack
        Layout.fillWidth: true
        Layout.alignment: Qt.AlignVCenter
        implicitHeight: Style.space(6)
        radius: height / 2
        color: Qt.darker(root.foreground, 3.2)

        Rectangle {
          width: meterTrack.width * Model.levelFraction(podRow.pod.level)
          height: parent.height
          radius: parent.radius
          color: podRow.low ? root.urgent : root.foreground
        }
      }

      Text {
        textFormat: Text.PlainText
        text: Model.levelText(podRow.pod.level)
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        horizontalAlignment: Text.AlignRight
        Layout.preferredWidth: Style.space(38)
      }

      Text {
        textFormat: Text.PlainText
        text: podRow.metaText
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
        Layout.preferredWidth: Style.space(56)
      }
    }
  }

  component ModeRow: CursorSurface {
    id: modeRow
    property int mode: 0

    readonly property string rowName: "mode:" + mode
    readonly property bool selected: pods.noiseMode === mode

    hasCursor: root.rowHasCursor(rowName)
    foreground: root.foreground
    implicitHeight: modeLabel.implicitHeight + Style.spacing.rowPaddingX

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onEntered: root.focusRow(modeRow.rowName)
      onClicked: pods.setNoiseMode(modeRow.mode)
    }

    RowLayout {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      spacing: Style.space(8)

      Text {
        textFormat: Text.PlainText
        id: modeLabel
        Layout.fillWidth: true
        text: Model.noiseModeName(modeRow.mode)
        color: root.foreground
        opacity: modeRow.selected ? 1.0 : 0.75
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        elide: Text.ElideRight
      }

      Text {
        textFormat: Text.PlainText
        Layout.alignment: Qt.AlignVCenter
        text: Model.GLYPH_CHECK
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.icon
        opacity: modeRow.selected ? 1.0 : 0.0
      }
    }
  }

  component ToggleRow: CursorSurface {
    id: toggleRow
    property string rowName: ""
    property string label: ""
    property string caption: ""
    property bool checked: false

    signal toggled()

    hasCursor: root.rowHasCursor(rowName)
    foreground: root.foreground
    implicitHeight: toggleContent.implicitHeight + Style.spacing.rowPaddingX

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onEntered: root.focusRow(toggleRow.rowName)
      onClicked: toggleRow.toggled()
    }

    RowLayout {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      spacing: Style.space(8)

      ColumnLayout {
        id: toggleContent
        Layout.fillWidth: true
        spacing: Style.space(1)

        Text {
          textFormat: Text.PlainText
          Layout.fillWidth: true
          text: toggleRow.label
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          elide: Text.ElideRight
        }

        Text {
          textFormat: Text.PlainText
          Layout.fillWidth: true
          text: toggleRow.caption
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }

      ToggleSwitch {
        Layout.alignment: Qt.AlignVCenter
        checked: toggleRow.checked
        busy: pods.busy
        hasCursor: toggleRow.hasCursor
        foreground: root.foreground
        onToggled: toggleRow.toggled()
      }
    }
  }

  component ValueRow: CursorSurface {
    id: valueRow
    property string rowName: ""
    property string label: ""
    property string value: ""

    hasCursor: root.rowHasCursor(rowName)
    foreground: root.foreground
    implicitHeight: valueLabel.implicitHeight + Style.spacing.rowPaddingX

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onEntered: root.focusRow(valueRow.rowName)
      onClicked: pods.cycleEarDetection()
    }

    RowLayout {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      spacing: Style.space(8)

      Text {
        textFormat: Text.PlainText
        id: valueLabel
        Layout.fillWidth: true
        text: valueRow.label
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        elide: Text.ElideRight
      }

      Text {
        textFormat: Text.PlainText
        text: valueRow.value
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        elide: Text.ElideRight
      }
    }
  }

  component SliderRow: Item {
    id: sliderRow
    implicitHeight: sliderColumn.implicitHeight

    Column {
      id: sliderColumn
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      spacing: Style.space(4)

      Row {
        width: parent.width
        spacing: Style.space(8)

        Text {
          textFormat: Text.PlainText
          text: "Adaptive noise"
          color: root.foreground
          opacity: 0.6
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        Item {
          width: Math.max(0, parent.width - parent.children[0].implicitWidth - parent.children[2].implicitWidth - parent.spacing * 2)
          height: 1
        }

        Text {
          textFormat: Text.PlainText
          text: pods.adaptiveNoiseLevel + "%"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }
      }

      PanelSlider {
        width: parent.width
        bar: root.bar
        minimum: 0
        maximum: 100
        step: root.adaptiveStepPercent
        integer: true
        tickCount: 5
        value: pods.adaptiveNoiseLevel
        onReleased: function (v) { pods.setAdaptiveNoiseLevel(v) }
      }
    }
  }
}
