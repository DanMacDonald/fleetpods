import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ColumnLayout {
  id: root
  property string peerName: ""
  property bool incoming: false
  property string stage: "leave"
  property bool busy: false
  property string error: ""
  property color foreground: "#e6e6e6"
  property color background: "#1a1a1e"
  property color urgent: "#f7768e"
  property string fontFamily: "sans-serif"
  signal leaveAndJoin()
  signal inviteInstead()
  signal dismiss()
  spacing: 16

  RowLayout {
    Layout.fillWidth: true
    Text {
      Layout.fillWidth: true
      text: root.stage === "leave" ? "Join " + root.peerName + "’s fleet" : "Invite to your fleet"
      textFormat: Text.PlainText
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: 15
      wrapMode: Text.WordWrap
    }
    ChoiceButton {
      text: "×"
      enabled: !root.busy
      Accessible.name: "Close fleet choice"
      onClicked: root.dismiss()
    }
  }
  Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Qt.alpha(root.foreground, 0.2) }
  Text {
    objectName: "fleetChoiceMessage"
    Layout.fillWidth: true
    text: root.stage === "leave" ? "You are currently in a fleet. Leave it?"
      : "Invite " + root.peerName + " to your fleet instead?"
    textFormat: Text.PlainText
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: 12
    wrapMode: Text.WordWrap
  }
  Text {
    Layout.fillWidth: true
    visible: root.stage === "leave"
    text: "This leaves the fleet on this computer only. Your AirPods pairing stays intact."
    textFormat: Text.PlainText
    color: Qt.alpha(root.foreground, 0.7)
    font.family: root.fontFamily
    font.pixelSize: 11
    wrapMode: Text.WordWrap
  }
  Text {
    Layout.fillWidth: true
    visible: root.error !== ""
    text: root.error
    textFormat: Text.PlainText
    color: root.urgent
    font.family: root.fontFamily
    font.pixelSize: 12
    wrapMode: Text.WordWrap
  }
  RowLayout {
    Layout.fillWidth: true
    Item { Layout.fillWidth: true }
    ChoiceButton {
      objectName: "fleetChoiceNo"
      text: "No"
      enabled: !root.busy
      onClicked: {
        root.error = ""
        if (root.stage === "leave" && !root.incoming) root.stage = "invite"
        else root.dismiss()
      }
    }
    ChoiceButton {
      objectName: "fleetChoiceYes"
      text: root.busy ? "Please wait…" : "Yes"
      enabled: !root.busy
      onClicked: root.stage === "leave" ? root.leaveAndJoin() : root.inviteInstead()
    }
  }
  component ChoiceButton: Button {
    id: button
    implicitWidth: Math.max(50, label.implicitWidth + 24)
    implicitHeight: 32
    contentItem: Text {
      id: label
      text: button.text
      textFormat: Text.PlainText
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: 12
      horizontalAlignment: Text.AlignHCenter
      verticalAlignment: Text.AlignVCenter
    }
    background: Rectangle { color: root.background; border.color: root.foreground; opacity: button.enabled ? 1 : 0.4 }
  }
}
