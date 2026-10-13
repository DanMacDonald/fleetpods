import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ColumnLayout {
  id: root
  property var pairing: ({ phase: "idle" })
  property color foreground: "#e6e6e6"
  property color background: "#1a1a1e"
  property color accent: foreground
  property color urgent: "#f7768e"
  property real nowMs: Date.now()
  readonly property int remainingSeconds: Math.max(0, Math.ceil((Number(pairing.expires_ms || 0) - nowMs) / 1000))
  readonly property string expiryText: "EXPIRES IN " + String(Math.floor(remainingSeconds / 60)).padStart(2, "0")
    + ":" + String(remainingSeconds % 60).padStart(2, "0")
  property string fontFamily: "sans-serif"
  property string codeFontFamily: "monospace"
  property bool busy: false
  property bool submittingCode: false
  readonly property int codeAttempt: Number(pairing.code_attempt || 0)
  property alias codeInput: codeInput
  readonly property string phase: String(pairing.phase || "idle")
  readonly property bool incoming: phase === "incoming"
  readonly property bool handshaking: ["exchanging", "waiting_ack", "waiting_commit", "waiting_done"].indexOf(phase) >= 0
  readonly property bool active: ["connecting", "inviting", "incoming", "exchanging",
    "waiting_offer", "waiting_ack", "waiting_commit", "waiting_done"].indexOf(phase) >= 0
  readonly property bool cancellable: active && !handshaking && !submittingCode
  function closeDialog() {
    if (cancellable) cancel()
    else dismiss()
  }
  Keys.onEscapePressed: closeDialog()
  signal submit(string code)
  signal cancel()
  signal dismiss()
  spacing: 16
  function submitCode() {
    if (!incoming || busy || submittingCode || codeInput.text.length !== 6) return
    submittingCode = true
    submit(codeInput.text)
  }
  onPhaseChanged: {
    codeInput.text = ""
    if (incoming) {
      submittingCode = false
      Qt.callLater(function() { if (root.visible && root.incoming) codeInput.forceActiveFocus() })
    }
  }
  onBusyChanged: if (!busy) submitCode()
  onCodeAttemptChanged: {
    codeInput.text = ""
    submittingCode = false
    Qt.callLater(function() { if (root.visible && root.incoming) codeInput.forceActiveFocus() })
  }
  onVisibleChanged: { nowMs = Date.now(); if (!visible) codeInput.text = "" }
  onPairingChanged: nowMs = Date.now()
  Timer {
    interval: 250
    repeat: true
    running: root.visible && root.active
    onTriggered: root.nowMs = Date.now()
  }

  RowLayout {
    Layout.fillWidth: true
    spacing: 10
    FleetIcon {
      visible: root.phase === "success" || root.handshaking
      Layout.preferredWidth: 24
      Layout.preferredHeight: 24
      ink: root.foreground
      paired: root.phase === "success"
    }
    Text {
      objectName: "fleetHeading"
      Layout.fillWidth: true
      text: root.incoming || root.phase === "waiting_offer" ? "Join " + root.pairing.peer_name + "'s fleet"
        : root.phase === "success" ? "Fleet joined"
        : root.phase === "error" ? "Could not join fleet"
        : root.phase === "expired" ? "Invitation expired"
        : root.phase === "cancelled" ? "Invitation cancelled"
        : root.pairing.initiator ? "Add to fleet" : "Joining fleet…"
      textFormat: Text.PlainText
      color: root.foreground
      font.family: root.incoming || root.phase === "waiting_offer" ? root.codeFontFamily : root.fontFamily
      font.bold: root.incoming || root.phase === "waiting_offer"
      font.pixelSize: 15
      wrapMode: Text.WordWrap
    }
    Button {
      objectName: "fleetClose"
      Layout.preferredWidth: 28
      Layout.preferredHeight: 28
      Accessible.name: "Close fleet invitation"
      hoverEnabled: true
      HoverHandler { cursorShape: Qt.PointingHandCursor }
      onClicked: root.closeDialog()
      contentItem: Text {
        text: "×"
        color: root.foreground
        opacity: 0.7
        font.pixelSize: 18
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
      }
      background: Rectangle { color: parent.hovered ? Qt.alpha(root.foreground, 0.08) : "transparent" }
    }
  }
  Rectangle {
    Layout.fillWidth: true
    implicitHeight: 1
    color: Qt.alpha(root.foreground, root.incoming ? 0.5 : 0.2)
  }
  Text {
    objectName: "fleetMessage"
    Layout.fillWidth: true
    text: root.incoming ? root.pairing.peer_name + " is inviting this device to join its fleet.\nEnter the six-digit code shown on " + root.pairing.peer_name + "."
      : root.phase === "success" ? "This device and " + root.pairing.peer_name + " now share a fleet."
      : root.phase === "error" ? root.pairing.error || "Please start a new invitation."
      : root.phase === "connecting" ? "Contacting " + root.pairing.peer_name + "…"
      : root.phase === "waiting_offer" ? "Receiving the invitation from " + root.pairing.peer_name + "…"
      : root.phase === "inviting" ? "Enter this code on " + root.pairing.peer_name + " to authorize the device."
      : root.active ? "Finishing fleet setup…"
      : "Start a new invitation to try again."
    textFormat: Text.PlainText
    color: root.foreground
    opacity: 0.85
    font.family: root.incoming ? root.codeFontFamily : root.fontFamily
    font.pixelSize: root.incoming ? 11 : 12
    wrapMode: Text.WordWrap
  }
  Rectangle {
    objectName: "fleetCodeCard"
    Layout.fillWidth: true
    implicitHeight: 110
    visible: root.phase === "inviting" && !!root.pairing.code
    color: "transparent"
    border.color: Qt.alpha(root.foreground, 0.25)
    Column {
      anchors.centerIn: parent
      spacing: 12
      Text {
        anchors.horizontalCenter: parent.horizontalCenter
        text: "PAIRING CODE"
        color: Qt.alpha(root.foreground, 0.6)
        font.family: root.fontFamily
        font.pixelSize: 10
      }
      Text {
        objectName: "fleetDisplayCode"
        anchors.horizontalCenter: parent.horizontalCenter
        text: String(root.pairing.code || "").slice(0, 3) + " " + String(root.pairing.code || "").slice(3)
        textFormat: Text.PlainText
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: 24
        font.letterSpacing: 1
      }
      Text {
        objectName: "fleetCodeExpiry"
        anchors.horizontalCenter: parent.horizontalCenter
        visible: Number(root.pairing.expires_ms || 0) > 0
        text: root.expiryText
        color: Qt.alpha(root.foreground, 0.7)
        font.family: root.fontFamily
        font.pixelSize: 10
      }
    }
  }
  RowLayout {
    Layout.fillWidth: true
    visible: root.phase === "inviting" || root.handshaking
    spacing: 10
    Text {
      text: "↻"
      color: root.accent
      font.pixelSize: 16
      RotationAnimator on rotation {
        from: 0; to: 360; duration: 1400; loops: Animation.Infinite
        running: root.visible && (root.phase === "inviting" || root.handshaking)
      }
    }
    Text {
      objectName: "fleetWaiting"
      Layout.fillWidth: true
      text: root.phase === "exchanging" ? "Performing fleet handshake…"
        : root.phase === "waiting_ack" ? "Sharing fleet membership with " + root.pairing.peer_name + "…"
        : root.phase === "waiting_commit" ? "Waiting for " + root.pairing.peer_name + " to confirm the fleet…"
        : root.phase === "waiting_done" ? "Waiting for final confirmation from " + root.pairing.peer_name + "…"
        : "Waiting for " + root.pairing.peer_name + "…"
      textFormat: Text.PlainText
      color: Qt.alpha(root.foreground, 0.8)
      font.family: root.fontFamily
      font.pixelSize: 12
      wrapMode: Text.WordWrap
    }
  }
  TextField {
    id: codeInput
    objectName: "fleetCodeInput"
    Layout.fillWidth: true
    visible: root.incoming
    enabled: root.incoming && !root.busy && !root.submittingCode
    readonly property int digitGap: 12
    readonly property real digitSize: Math.min(40, (width - 5 * digitGap) / 6)
    implicitHeight: digitSize
    padding: 0
    Accessible.name: "Six-digit fleet code"
    maximumLength: 6
    validator: RegularExpressionValidator { regularExpression: /[0-9]{0,6}/ }
    inputMethodHints: Qt.ImhDigitsOnly | Qt.ImhNoPredictiveText
    font.family: root.codeFontFamily
    font.pixelSize: 24
    // One editor preserves native paste, selection and keyboard navigation across all six boxes.
    color: "transparent"
    selectionColor: "transparent"
    selectedTextColor: "transparent"
    cursorDelegate: Item {}
    background: null
    Row {
      anchors.centerIn: parent
      spacing: codeInput.digitGap
      z: 1
      Repeater {
        model: 6
        delegate: Rectangle {
          required property int index
          objectName: "fleetCodeDigit" + index
          readonly property string digit: codeInput.text.charAt(index)
          readonly property bool selected: index >= codeInput.selectionStart && index < codeInput.selectionEnd
          readonly property bool current: codeInput.activeFocus && (codeInput.selectionStart !== codeInput.selectionEnd
            ? selected : index === Math.min(codeInput.cursorPosition, 5))
          width: codeInput.digitSize
          height: width
          color: selected ? Qt.alpha(root.foreground, 0.12) : root.background
          border.color: current ? root.accent : Qt.alpha(root.foreground, 0.4)
          border.width: current ? 2 : 1
          opacity: codeInput.enabled ? 1 : 0.4
          Text {
            anchors.centerIn: parent
            text: parent.digit
            color: root.foreground
            font.family: root.codeFontFamily
            font.pixelSize: 24
          }
          Rectangle {
            anchors.centerIn: parent
            width: 1
            height: 22
            color: root.foreground
            visible: parent.current && parent.digit === "" && codeInput.selectionStart === codeInput.selectionEnd
            SequentialAnimation on opacity {
              running: parent.visible
              loops: Animation.Infinite
              NumberAnimation { to: 1; duration: 0 }
              PauseAnimation { duration: 500 }
              NumberAnimation { to: 0; duration: 0 }
              PauseAnimation { duration: 500 }
            }
          }
          MouseArea {
            anchors.fill: parent
            cursorShape: Qt.IBeamCursor
            onClicked: {
              codeInput.forceActiveFocus()
              if (index < codeInput.text.length) codeInput.select(index, index + 1)
              else codeInput.cursorPosition = codeInput.text.length
            }
          }
        }
      }
    }
    onTextEdited: root.submitCode()
    onAccepted: root.submitCode()
    Keys.onEscapePressed: root.closeDialog()
  }
  Text {
    objectName: "fleetCodeError"
    Layout.fillWidth: true
    visible: root.incoming && !!root.pairing.error
    text: root.pairing.error || ""
    textFormat: Text.PlainText
    color: root.urgent
    font.family: root.fontFamily
    font.pixelSize: 12
    wrapMode: Text.WordWrap
  }
  Text {
    Layout.fillWidth: true
    visible: root.incoming
    text: Number(root.pairing.expires_ms || 0) > 0 ? root.expiryText : "The code expires after two minutes."
    color: root.foreground
    opacity: 0.6
    font.family: root.codeFontFamily
    font.pixelSize: 12
  }
  RowLayout {
    Layout.fillWidth: true
    visible: !root.active || (root.cancellable && root.phase !== "inviting")
    spacing: 12
    Item { Layout.fillWidth: true }
    FleetButton {
      objectName: "fleetCancel"
      text: root.active ? "Cancel" : "Done"
      onClicked: root.closeDialog()
    }
  }
  component FleetButton: Button {
    id: control
    hoverEnabled: true
    HoverHandler { cursorShape: Qt.PointingHandCursor }
    implicitWidth: Math.max(72, label.implicitWidth + 24)
    implicitHeight: 32
    contentItem: Text {
      id: label
      text: control.text
      color: root.foreground
      opacity: control.enabled ? 1 : 0.4
      font.family: root.incoming ? root.codeFontFamily : root.fontFamily
      font.pixelSize: 12
      horizontalAlignment: Text.AlignHCenter
      verticalAlignment: Text.AlignVCenter
    }
    background: Rectangle {
      color: control.down ? Qt.lighter(root.background, 1.6) : root.background
      border.color: root.foreground
      opacity: control.enabled ? 1 : 0.4
    }
  }
}
