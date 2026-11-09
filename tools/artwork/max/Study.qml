import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Window
import "../../../plugin" as Pods

Window {
  id: window
  visible: true
  width: 980
  height: 780
  title: "FleetPods · AirPods Max artwork study"
  property bool lightTheme: Qt.application.arguments.indexOf('--light') >= 0
  // Resting view on the same side as the frontal worn pose.
  readonly property real selectedYaw: Math.PI - 2.6
  property real yaw: selectedYaw
  property real roll: 0.133
  property real resting: 1
  readonly property color background: lightTheme ? '#efeee8' : '#1b1c28'
  readonly property color foreground: lightTheme ? '#343b4c' : '#afb4d8'
  readonly property color muted: lightTheme ? '#697185' : '#868ca8'
  color: background

  component Caption: Text {
    color: window.foreground
    textFormat: Text.PlainText
    font.pixelSize: 13
  }

  Rectangle {
    id: content
    anchors.fill: parent
    color: window.background
    ColumnLayout {
      anchors.fill: parent
      anchors.margins: 28
      spacing: 16
      Caption { text: 'AirPods Max · artwork study'; font.pixelSize: 25 }
      Caption { text: 'Panel dot painter, lighting and palette · cups close inward off head'; color: window.muted }
      RowLayout {
        Layout.fillWidth: true
        Button { text: window.lightTheme ? 'Dark theme' : 'Light theme'; onClicked: window.lightTheme = !window.lightTheme }
        Caption { text: 'Rotate' }
        Slider { Layout.fillWidth: true; from: -Math.PI; to: Math.PI; value: window.yaw; onMoved: window.yaw = value }
        Button { text: 'Front'; onClicked: { window.yaw = 0; window.roll = 0 } }
        Button { text: 'Selected angle'; onClicked: { window.yaw = window.selectedYaw; window.roll = 0.133 } }
      }
      RowLayout {
        Layout.fillWidth: true
        Caption { text: 'Off-head pose' }
        Slider { Layout.fillWidth: true; from: 0; to: 1; value: window.resting; onMoved: window.resting = value }
        Caption { text: Math.round(window.resting * 100) + '%' }
      }
      RowLayout {
        Layout.fillWidth: true
        spacing: 20
        Repeater {
          model: 3
          ColumnLayout {
            id: column
            required property int index
            Layout.fillWidth: true
            Layout.preferredWidth: 290
            spacing: 8
            Caption { Layout.alignment: Qt.AlignHCenter; text: ['WORN', 'OFF HEAD · CONNECTED', 'AIRPODS REFERENCE'][column.index] }
            Item {
              Layout.alignment: Qt.AlignHCenter
              Layout.preferredWidth: 280; Layout.preferredHeight: 280
              Pods.MaxArtwork {
                anchors.fill: parent
                visible: column.index < 2
                foregroundColor: window.foreground; backgroundColor: window.background
                yaw: window.yaw
                roll: window.roll
                animatePose: false
                rest: column.index === 1 ? window.resting : 0
              }
              Pods.MeshArtwork {
                anchors.fill: parent
                visible: column.index === 2
                foregroundColor: window.foreground; backgroundColor: window.background
                artworkScale: 1.38
              }
            }
            Item {
              Layout.alignment: Qt.AlignHCenter
              Layout.preferredWidth: 137; Layout.preferredHeight: 137
              Pods.MaxArtwork {
                anchors.fill: parent
                visible: column.index < 2
                foregroundColor: window.foreground; backgroundColor: window.background
                yaw: window.yaw
                roll: window.roll
                animatePose: false
                rest: column.index === 1 ? window.resting : 0
              }
              Pods.MeshArtwork {
                anchors.fill: parent
                visible: column.index === 2
                foregroundColor: window.foreground; backgroundColor: window.background
                artworkScale: 1.38
              }
            }
          }
        }
      }
      Caption { text: 'Lower row: 137 px frames · physical dot grid follows display scaling'; color: window.muted }
      Item { Layout.fillHeight: true }
      Caption { text: 'Max mesh: Mr.Philin (yarik.chilin) · CC BY 4.0 · simplified and posed for this study'; font.pixelSize: 11; color: window.muted }
    }
  }

  Timer {
    interval: 1800
    running: Qt.application.arguments.indexOf('--capture') >= 0
    onTriggered: content.grabToImage(function(result) {
      var path = window.lightTheme ? '/tmp/fleetpods-max-study-light.png' : '/tmp/fleetpods-max-study.png'
      if (!result.saveToFile(path)) { console.error('Capture failed:', path); Qt.exit(1); return }
      console.log('Saved', path)
      Qt.quit()
    })
  }
}
