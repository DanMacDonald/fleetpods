import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Window
import ".." as Pods
import "../DotGrid.js" as Grid

Window {
  id: window
  width: 760
  height: 510
  visible: true
  title: "FleetPods · Pods out animation"
  property bool lightTheme: Qt.application.arguments.indexOf("--light") >= 0
  property bool capture: Qt.application.arguments.indexOf("--capture") >= 0
  property real arrival: 1
  property real dotSpacing: Grid.pitch
  readonly property color foreground: lightTheme ? "#343b4c" : "#afb4d8"
  readonly property color background: lightTheme ? "#efeee8" : "#191b27"
  color: background

  function replay() { drop.restart() }
  Component.onCompleted: if (!capture) replay()
  NumberAnimation {
    id: drop
    target: window; property: "arrival"
    from: 0; to: 1; duration: 1000; easing.type: Easing.OutCubic
  }

  Rectangle {
    id: content
    anchors.fill: parent
    color: window.background
    ColumnLayout {
      anchors.fill: parent
      anchors.margins: 26
      spacing: 12
      Text {
        text: "Pods out · drop and turn"
        textFormat: Text.PlainText
        color: window.foreground; font.pixelSize: 23
      }
      RowLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: 40
        ColumnLayout {
          Layout.fillWidth: true
          Text { text: "Current 2D pose"; color: window.foreground; textFormat: Text.PlainText }
          Pods.PixelPods {
            Layout.alignment: Qt.AlignHCenter
            Layout.preferredWidth: 280; Layout.preferredHeight: 280
            foregroundColor: window.foreground; backgroundColor: window.background
          }
        }
        ColumnLayout {
          Layout.fillWidth: true
          Text { text: "3D · tips inward, then turn"; color: window.foreground; textFormat: Text.PlainText }
          Item {
            Layout.alignment: Qt.AlignHCenter
            Layout.preferredWidth: 280; Layout.preferredHeight: 280
            clip: true
            Pods.BudsMesh {
              width: parent.width; height: parent.height
              y: -(1 - window.arrival) * height
              arrival: window.arrival
              dotPitch: window.dotSpacing
              foregroundColor: window.foreground; backgroundColor: window.background
            }
          }
        }
      }
      RowLayout {
        Button { text: "Replay entrance"; onClicked: window.replay() }
        Button { text: window.lightTheme ? "Dark" : "Light"; onClicked: window.lightTheme = !window.lightTheme }
        Text { text: "Entrance"; color: window.foreground; textFormat: Text.PlainText }
        Slider {
          Layout.fillWidth: true
          from: 0; to: 1; value: window.arrival
          onMoved: { drop.stop(); window.arrival = value }
        }
      }
      RowLayout {
        Text { text: "Dot spacing"; color: window.foreground; textFormat: Text.PlainText }
        Slider {
          Layout.fillWidth: true
          from: 2; to: 8; stepSize: 0.5
          value: window.dotSpacing
          Accessible.name: "Dot spacing"
          onMoved: window.dotSpacing = value
        }
        Text {
          text: window.dotSpacing.toFixed(1) + " px"
          color: window.foreground; textFormat: Text.PlainText
          Layout.preferredWidth: 50
        }
      }
    }
  }
  Timer {
    interval: 1200; running: window.capture
    onTriggered: content.grabToImage(function(result) {
      result.saveToFile(window.lightTheme ? "/tmp/fleetpods-pods-out-light.png" : "/tmp/fleetpods-pods-out.png")
      Qt.quit()
    })
  }
}
