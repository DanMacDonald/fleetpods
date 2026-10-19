import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import "Model.js" as Model

Rectangle {
  id: root

  required property var messages
  required property color foreground
  required property color urgent
  required property string fontFamily
  property string playbackSummary: ""
  // Device names in a log line are drawn in this colour so they stand apart from the message.
  property var deviceNames: []
  property color nameColor: foreground
  property color background: Commons.Color.popups.background
  readonly property bool lightMode: background.hslLightness > foreground.hslLightness
  // Monochrome themes still need the log's semantic colors.
  readonly property color errorColor: lightMode && urgent.hslSaturation < 0.1 ? "#b42332" : urgent
  readonly property color deviceColor: lightMode && nameColor.hslSaturation < 0.1 ? "#245fa8" : nameColor
  readonly property color audioColor: lightMode ? "#245fa8" : "#6aa9ff"
  property string guidanceText: ""
  readonly property bool showingGuidance: guidanceText !== ""
  property bool expanded: false
  onShowingGuidanceChanged: if (showingGuidance) expanded = false
  readonly property var latest: messages.count > 0 ? messages.get(messages.count - 1) : null
  readonly property int padding: Style.space(12)
  readonly property color muted: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.6)
  readonly property color ruleColor: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.18)
  readonly property color success: Qt.hsla(0.4, 0.35, Math.max(0.4, Math.min(0.8, foreground.hslLightness)), 1)
  readonly property real collapsedHeight: Math.max(Style.space(22), latestStatus.implicitHeight) + padding * 2
  implicitHeight: showingGuidance ? Math.max(collapsedHeight, statusGuidance.implicitHeight + padding)
    : expanded ? activityHeading.y + activityHeading.height + Style.space(14)
    + Math.min(log.contentHeight, Style.space(180)) + padding : collapsedHeight
  // Drives the dots on a connection attempt still under way.
  property int dots: 3
  property real nowMs: Date.now()

  function messageText(row) {
    if (!row) return "No recent activity"
    if (row.live && row.retryPeer) {
      var seconds = Math.max(0, Math.ceil((row.retryAt - root.nowMs) / 1000))
      var label = seconds > 0 ? "Waiting for " + row.retryPeer + ". Retrying in " + seconds + "s" : "Retrying AirPods connection"
      return label + (".".repeat(root.dots) + "   ").slice(0, 3)
    }
    if (row.live && row.eventKind === "waitingDisconnect")
      return row.message.slice(0, -3) + ".".repeat(root.dots)
    if (row.live) return Model.connectingLine(row.detail, root.dots, false, row.device)
    if (row.eventKind === "connected") return "Connected to AirPods"
    if (row.eventKind === "connectingAgain") return "Retrying connection"
    return row.message
  }

  function styledText(row) {
    // StyledText collapses ordinary padding spaces. Preserve the three dot
    // positions so the retry count stays put throughout the animation.
    var text = Model.deviceNameMarkup(messageText(row), root.deviceNames, root.deviceColor)
    if (row && row.live && row.detail && !row.retryPeer)
      text = text.replace(/ {2,}/g, function (spaces) { return "&nbsp;".repeat(spaces.length) })
    return text
  }

  function messageState(row) {
    if (!row) return "idle"
    if (row.eventKind === "caseClosed") return "caseClosed"
    if (row.kind === "success") return "success"
    if (row.kind === "error") return "error"
    if (row.message === "Trying to reconnect the Apple device...") return "apple"
    if (row.kind === "audio" && (row.eventKind === "audioTaken" || row.eventKind === "audioOn")) return "audio"
    if (row.kind === "status" && row.eventKind === "pairedAlongside") return "fleetPair"
    if (row.kind === "status" && row.eventKind === "fleetSwitched") return "fleet"
    if (row.live || row.eventKind === "connectingAgain") return "retry"
    if (row.eventKind === "connected") return "success"
    return "idle"
  }

  Timer {
    interval: 400
    repeat: true
    running: root.visible && !root.showingGuidance
    onTriggered: { root.dots = root.dots % 3 + 1; root.nowMs = Date.now() }
  }
  color: showingGuidance ? "transparent" : Qt.rgba(foreground.r, foreground.g, foreground.b, 0.035)

  Text {
    id: statusGuidance
    objectName: "statusGuidance"
    visible: root.showingGuidance
    anchors.verticalCenter: parent.verticalCenter
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.leftMargin: root.padding
    anchors.rightMargin: root.padding
    text: root.guidanceText
    textFormat: Text.PlainText
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.space(11)
    wrapMode: Text.WordWrap
    horizontalAlignment: Text.AlignHCenter
  }

  Rectangle {
    visible: !root.showingGuidance
    anchors.top: parent.top
    width: parent.width
    height: 1
    color: root.ruleColor
  }

  Rectangle {
    visible: !root.showingGuidance
    anchors.bottom: parent.bottom
    width: parent.width
    height: 1
    color: root.ruleColor
  }

  AbstractButton {
    id: toggle
    visible: !root.showingGuidance
    objectName: "statusToggle"
    x: root.padding
    y: root.padding
    width: parent.width - root.padding * 2
    height: headerText.implicitHeight + headerText.y * 2
    hoverEnabled: true
    Accessible.name: root.expanded ? "Collapse recent activity" : "Expand recent activity"
    onClicked: {
      root.expanded = !root.expanded
      if (root.expanded) {
        log.followLatest = true
        Qt.callLater(log.scrollToLatest)
      }
    }
    background: Rectangle {
      radius: Style.space(3)
      color: root.foreground
      opacity: toggle.hovered || toggle.activeFocus ? 0.04 : 0
    }
    contentItem: Item {
      StatusSymbol {
        id: headerSymbol
        objectName: "headerSymbol"
        anchors.left: parent.left
        y: headerText.y + (latestStatus.height - height) / 2
        stateName: root.messageState(root.latest)
        active: root.latest !== null && root.latest.live
      }
      Column {
        id: headerText
        anchors.left: headerSymbol.right
        anchors.leftMargin: Style.space(10)
        anchors.right: indicator.left
        anchors.rightMargin: Style.space(12)
        y: Math.max(0, (Style.space(22) - latestStatus.height) / 2)
        spacing: Style.space(4)
        Text {
          id: latestStatus
          objectName: "latestStatus"
          property bool initialized: false
          property real entranceProgress: 1
          readonly property string message: root.latest ? root.latest.message : ""
          readonly property int entranceDuration: 450
          readonly property real entranceDistance: Style.space(8)
          Component.onCompleted: initialized = true
          onMessageChanged: if (initialized && root.visible && message !== "") entrance.restart()
          onVisibleChanged: if (!visible) { entrance.stop(); entranceProgress = 1 }
          opacity: 0.2 + entranceProgress * 0.8
          transform: Translate { y: (1 - latestStatus.entranceProgress) * latestStatus.entranceDistance }
          NumberAnimation {
            id: entrance
            target: latestStatus
            property: "entranceProgress"
            from: 0
            to: 1
            duration: latestStatus.entranceDuration
            easing.type: Easing.OutCubic
          }
          width: parent.width
          text: root.styledText(root.latest)
          textFormat: Text.StyledText
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.space(11)
          elide: Text.ElideRight
        }
        Text {
          visible: root.expanded && text !== ""
          width: parent.width
          text: root.playbackSummary
          textFormat: Text.PlainText
          color: root.muted
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }
      Item {
        id: indicator
        anchors.right: parent.right
        y: headerText.y + (latestStatus.height - height) / 2
        width: Style.space(12)
        height: width
        rotation: root.expanded ? 180 : 0
        Rectangle {
          width: parent.width * 0.6; height: Style.space(1)
          x: 0; y: parent.height / 2
          rotation: 45
          color: root.muted
          antialiasing: true
        }
        Rectangle {
          width: parent.width * 0.6; height: Style.space(1)
          x: parent.width * 0.4; y: parent.height / 2
          rotation: -45
          color: root.muted
          antialiasing: true
        }
      }
    }
  }

  Rectangle {
    id: divider
    visible: root.expanded && !root.showingGuidance
    y: toggle.y + toggle.height + Style.space(16)
    width: parent.width
    height: 1
    color: root.ruleColor
  }

  Item {
    id: activityHeading
    visible: root.expanded && !root.showingGuidance
    x: root.padding
    y: divider.y + divider.height + Style.space(16)
    width: parent.width - root.padding * 2
    height: caption.implicitHeight
    Text {
      id: caption
      text: "RECENT ACTIVITY"
      color: root.muted
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
    Text {
      objectName: "statusEventCount"
      anchors.right: parent.right
      text: root.messages.count + (root.messages.count === 1 ? " event" : " events")
      color: root.muted
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }

  ListView {
    id: log
    objectName: "statusMessages"
    anchors.top: activityHeading.bottom
    anchors.topMargin: Style.space(14)
    anchors.bottom: parent.bottom
    anchors.bottomMargin: root.padding
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.leftMargin: root.padding
    anchors.rightMargin: root.padding
    clip: true
    visible: root.expanded && !root.showingGuidance
    model: root.messages
    spacing: Style.space(14)
    boundsBehavior: Flickable.StopAtBounds
    property bool followLatest: true

    function scrollToLatest() {
      if (followLatest) positionViewAtBeginning()
    }

    onContentYChanged: if (moving || dragging) followLatest = atYBeginning
    onMovementEnded: followLatest = atYBeginning
    onCountChanged: Qt.callLater(log.scrollToLatest)
    onContentHeightChanged: Qt.callLater(log.scrollToLatest)
    onHeightChanged: Qt.callLater(log.scrollToLatest)

    WheelHandler {
      target: null
      acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
      readonly property real scrollMultiplier: 2
      readonly property real wheelStep: Style.space(40)
      onWheel: function(event) {
        var delta = event.pixelDelta.y || event.angleDelta.y / 120 * wheelStep
        if (!delta) { event.accepted = false; return }
        log.cancelFlick()
        var bottom = log.originY + Math.max(0, log.contentHeight - log.height)
        log.contentY = Math.max(log.originY, Math.min(bottom, log.contentY - delta * scrollMultiplier))
        log.followLatest = log.atYBeginning
        event.accepted = true
      }
    }

    ScrollBar.vertical: ScrollBar {
      policy: ScrollBar.AsNeeded
      onPositionChanged: if (pressed) Qt.callLater(function () { log.followLatest = log.atYBeginning })
    }

    delegate: Item {
      id: entry
      required property int index
      readonly property var row: index < root.messages.count ? root.messages.get(root.messages.count - index - 1) : null
      width: log.width - Style.space(10)
      height: body.implicitHeight + Style.space(4) + stamp.implicitHeight

      StatusSymbol {
        id: symbol
        objectName: "statusSymbol"
        anchors.left: parent.left
        y: (body.implicitHeight - height) / 2
        stateName: root.messageState(entry.row)
        active: entry.row !== null && entry.row.live
      }
      Text {
        id: body
        objectName: "statusLine"
        anchors.left: symbol.right
        anchors.leftMargin: Style.space(10)
        anchors.right: parent.right
        textFormat: Text.StyledText
        text: root.styledText(entry.row)
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.space(11)
        wrapMode: Text.Wrap
      }
      Text {
        id: stamp
        objectName: "statusTimestamp"
        anchors.left: body.left
        anchors.top: body.bottom
        anchors.topMargin: Style.space(4)
        text: entry.row ? entry.row.time : ""
        color: root.muted
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }

  component StatusSymbol: Item {
    id: statusSymbol
    property string stateName
    property bool active: false
    NumberAnimation on rotation {
      from: 0
      to: 360
      duration: 1000
      loops: Animation.Infinite
      running: statusSymbol.visible && statusSymbol.active && statusSymbol.stateName === "retry"
      onStopped: statusSymbol.rotation = 0
    }
    readonly property color ink: stateName === "error" ? root.errorColor : stateName === "success" ? root.success
      : stateName === "audio" ? root.audioColor : root.muted
    width: Style.space(14)
    height: width
    Rectangle {
      anchors.fill: parent
      visible: parent.stateName === "error" || parent.stateName === "success"
      radius: width / 2
      color: "transparent"
      border.width: Style.space(1)
      border.color: parent.ink
      antialiasing: true
    }
    Text {
      visible: parent.stateName !== "fleet" && parent.stateName !== "fleetPair"
        && parent.stateName !== "apple" && parent.stateName !== "caseClosed" && parent.stateName !== "audio"
      anchors.centerIn: parent
      text: parent.stateName === "error" ? "!" : parent.stateName === "success" ? "✓"
        : parent.stateName === "retry" ? "↻" : "·"
      color: parent.ink
      font.family: "sans-serif"
      font.pixelSize: Style.space(parent.stateName === "retry" ? 18 : 11)
    }
    Canvas {
      anchors.fill: parent
      visible: statusSymbol.stateName === "apple"
      readonly property color ink: root.foreground
      onInkChanged: requestPaint()
      onWidthChanged: requestPaint()
      onHeightChanged: requestPaint()
      onVisibleChanged: if (visible) requestPaint()
      onPaint: {
        var context = getContext("2d")
        context.reset()
        context.scale(width / 24, height / 24)
        context.strokeStyle = ink
        context.lineWidth = 1.5
        context.lineJoin = "round"
        context.beginPath()
        context.moveTo(12, 5.5)
        context.bezierCurveTo(11.7, 2.8, 14.1, 0.6, 16.5, 0.5)
        context.bezierCurveTo(16.8, 3.1, 14.7, 5.6, 12, 5.5)
        context.closePath()
        context.stroke()
        context.beginPath()
        context.moveTo(12, 7)
        context.bezierCurveTo(9.7, 7, 9.2, 5.6, 6.7, 6.1)
        context.bezierCurveTo(2.8, 6.8, 1.8, 10.6, 3, 15)
        context.bezierCurveTo(3.9, 18.5, 6.3, 23.2, 8.7, 23)
        context.bezierCurveTo(10.2, 22.9, 10.6, 22.1, 12, 22.1)
        context.bezierCurveTo(13.5, 22.1, 14.2, 23, 15.6, 22.9)
        context.bezierCurveTo(17.9, 22.8, 20, 19, 21, 16.5)
        context.bezierCurveTo(16.9, 14.9, 16.2, 10.1, 20, 7.8)
        context.bezierCurveTo(18.2, 5.6, 15.7, 5.6, 14, 6.4)
        context.bezierCurveTo(13.2, 6.7, 12.5, 7, 12, 7)
        context.closePath()
        context.stroke()
      }
    }
    // Two beamed notes drawn to the same box as the connected ring, so they line up in the log.
    Canvas {
      objectName: "audioNote"
      anchors.fill: parent
      visible: statusSymbol.stateName === "audio"
      readonly property color ink: statusSymbol.ink
      onInkChanged: requestPaint()
      onWidthChanged: requestPaint()
      onHeightChanged: requestPaint()
      onVisibleChanged: if (visible) requestPaint()
      onPaint: {
        var context = getContext("2d")
        context.reset()
        context.scale(width / 24, height / 24)
        context.fillStyle = ink
        context.strokeStyle = ink
        context.lineWidth = 1.8
        function head(x, y) {
          context.save()
          context.translate(x, y)
          context.rotate(-0.35)
          context.scale(1, 0.72)
          context.beginPath()
          context.arc(0, 0, 3.3, 0, Math.PI * 2)
          context.fill()
          context.restore()
        }
        head(6.4, 19.2)
        head(17.6, 17.2)
        context.beginPath()
        context.moveTo(9.4, 18.6)
        context.lineTo(9.4, 5.6)
        context.moveTo(20.6, 16.6)
        context.lineTo(20.6, 3.6)
        context.stroke()
        context.beginPath()
        context.moveTo(8.5, 4.6)
        context.lineTo(21.5, 2.2)
        context.lineTo(21.5, 5.4)
        context.lineTo(8.5, 7.8)
        context.closePath()
        context.fill()
      }
    }
    AirPodsIcon {
      visible: statusSymbol.stateName === "caseClosed"
      anchors.centerIn: parent
      iconSize: Style.space(13)
      color: root.foreground
      variant: "pro"
      caseState: "closed"
    }
    FleetIcon {
      visible: statusSymbol.stateName === "fleet" || statusSymbol.stateName === "fleetPair"
      anchors.centerIn: parent
      width: statusSymbol.width
      height: width
      ink: statusSymbol.ink
      paired: statusSymbol.stateName === "fleetPair"
    }
  }
}
