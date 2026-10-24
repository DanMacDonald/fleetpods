pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Window
import ".." as Pods
import "../DotGrid.js" as Grid

Window {
  id: window
  width: 1040
  height: 760
  visible: true
  title: "FleetPods · Dot mesh study"
  color: "#1c1d26"
  property color ink: "#c8cdda"
  property color muted: "#797f94"
  property string state: Qt.application.arguments.indexOf("--closed") >= 0 ? "closed"
    : Qt.application.arguments.indexOf("--right-out") >= 0 ? "open-left"
    : Qt.application.arguments.indexOf("--left-out") >= 0 ? "open-right"
    : Qt.application.arguments.indexOf("--empty") >= 0 ? "open-empty"
    : Qt.application.arguments.indexOf("--open") >= 0 ? "open" : ""
  property bool moving: Qt.application.arguments.indexOf("--capture") < 0
  property bool lightTheme: Qt.application.arguments.indexOf("--light") >= 0
  property bool highlighted: Qt.application.arguments.indexOf("--dim") < 0
  property bool hoverHighlight: false
  property real angle: -0.32
  property real spacing: Grid.pitch
  readonly property color background: lightTheme ? "#efeee8" : "#1c1d26"
  readonly property color foreground: lightTheme ? "#343b4c" : ink
  readonly property color shade: lightTheme ? "#717b8e" : "#465675"

  component Label: Text {
    textFormat: Text.PlainText
    font.family: "monospace"
    font.pixelSize: 12
    color: window.muted
  }

  component Choice: Rectangle {
    id: choice
    property string label
    property bool selected: false
    signal clicked()
    implicitWidth: caption.implicitWidth + 28
    implicitHeight: 36
    radius: 5
    color: selected ? (window.lightTheme ? "#dcdde0" : "#303443") : "transparent"
    border.width: 1
    border.color: selected ? "#697791" : (window.lightTheme ? "#c8cbd0" : "#373b4b")
    Text {
      id: caption
      anchors.centerIn: parent
      text: choice.label
      textFormat: Text.PlainText
      font.family: "monospace"
      font.pixelSize: 12
      color: choice.selected ? window.foreground : window.muted
    }
    MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: choice.clicked() }
  }

  component Dial: Slider {
    id: dial
    implicitHeight: 22
    background: Rectangle {
      x: dial.leftPadding
      y: dial.topPadding + dial.availableHeight / 2 - height / 2
      width: dial.availableWidth
      height: 2
      color: window.lightTheme ? "#c8cbd0" : "#373b4b"
      Rectangle { width: dial.visualPosition * parent.width; height: parent.height; color: "#7d8da9" }
    }
    handle: Rectangle {
      x: dial.leftPadding + dial.visualPosition * (dial.availableWidth - width)
      y: dial.topPadding + dial.availableHeight / 2 - height / 2
      width: 9; height: 9
      radius: 1
      color: window.foreground
    }
  }

  Rectangle {
    id: content
    anchors.fill: parent
    color: window.background

    ColumnLayout {
      anchors.fill: parent
      anchors.margins: 36
      spacing: 20

      RowLayout {
        Layout.fillWidth: true
        Label { text: "F L E E T P O D S"; color: window.foreground; font.bold: true }
        Item { Layout.fillWidth: true }
        Label { text: "ARTWORK STUDY / 01"; font.pixelSize: 10; font.letterSpacing: 1 }
      }
      Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: window.lightTheme ? "#d0d1d5" : "#343746" }

      RowLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: 40

        Item {
          Layout.fillWidth: true
          Layout.fillHeight: true
          Layout.preferredWidth: 560
          Layout.minimumWidth: 400
          Pods.DotPods {
            id: hero
            anchors.centerIn: parent
            width: Math.min(parent.width, parent.height)
            height: width
            foregroundColor: window.foreground
            highlighted: window.hoverHighlight ? rotationArea.containsMouse : window.highlighted
            backgroundColor: window.background
            shadowColor: window.shade
            caseState: window.state
            animating: window.moving && window.visibility !== Window.Minimized
            yaw: window.angle
            dotPitch: window.spacing
            MouseArea {
              id: rotationArea
              anchors.fill: parent
              hoverEnabled: true
              property real previousX: 0
              cursorShape: pressed ? Qt.ClosedHandCursor : Qt.OpenHandCursor
              onPressed: function(mouse) { previousX = mouse.x }
              onPositionChanged: function(mouse) {
                if (!pressed) return
                window.angle = Math.max(-Math.PI, Math.min(Math.PI, window.angle + (mouse.x - previousX) * 0.014))
                previousX = mouse.x
              }
            }
          }
          Label {
            anchors.bottom: parent.bottom
            anchors.horizontalCenter: parent.horizontalCenter
            text: window.moving ? "●  LIVE SURFACE" : "○  PAUSED"
            font.pixelSize: 10
            font.letterSpacing: 1.1
          }
        }

        ColumnLayout {
          Layout.preferredWidth: 330
          Layout.maximumWidth: 330
          spacing: 18
          Label { text: "AIRPODS PRO"; font.pixelSize: 10; font.letterSpacing: 1.5 }
          Text {
            text: "Light on a grid."
            textFormat: Text.PlainText
            color: window.foreground
            font.pixelSize: 30
            font.weight: Font.DemiBold
          }
          Text {
            Layout.fillWidth: true
            text: window.lightTheme ? "Shadows and contours drawn in ink.\nBright surfaces left open."
              : "A fixed matrix of square dots.\nA turning surface, shaded in depth."
            textFormat: Text.PlainText
            color: window.muted
            font.pixelSize: 14
            lineHeight: 1.4
          }
          Item { implicitHeight: 4 }
          RowLayout {
            spacing: 8
            Choice { label: "Pods"; selected: window.state === ""; onClicked: window.state = "" }
            Choice { label: "Open"; selected: window.state === "open"; onClicked: window.state = "open" }
            Choice { label: "Closed"; selected: window.state === "closed"; onClicked: window.state = "closed" }
          }
          RowLayout {
            Choice { label: window.moving ? "Pause" : "Animate"; onClicked: window.moving = !window.moving }
            Choice { label: window.lightTheme ? "Dark" : "Light"; onClicked: window.lightTheme = !window.lightTheme }
          }
          RowLayout {
            Choice { label: "Hover"; selected: window.hoverHighlight; onClicked: window.hoverHighlight = true }
            Choice {
              label: "Highlighted"; selected: !window.hoverHighlight && window.highlighted
              onClicked: { window.highlighted = true; window.hoverHighlight = false }
            }
            Choice {
              label: "Dim"; selected: !window.hoverHighlight && !window.highlighted
              onClicked: { window.highlighted = false; window.hoverHighlight = false }
            }
          }
          RowLayout {
            spacing: 8
            Choice { label: "Left out"; selected: window.state === "open-right"; onClicked: window.state = "open-right" }
            Choice { label: "Right out"; selected: window.state === "open-left"; onClicked: window.state = "open-left" }
            Choice { label: "Both out"; selected: window.state === "open-empty"; onClicked: window.state = "open-empty" }
          }
          Label { text: "ROTATION"; font.pixelSize: 10; font.letterSpacing: 1 }
          Dial {
            Layout.fillWidth: true
            from: -Math.PI; to: Math.PI; value: window.angle
            onMoved: window.angle = value
          }
          Label { text: "DOT SPACING"; font.pixelSize: 10; font.letterSpacing: 1 }
          Dial {
            Layout.fillWidth: true
            from: 2; to: 9; stepSize: 0.5; value: window.spacing
            onMoved: window.spacing = value
          }
        }
      }

      Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: window.lightTheme ? "#d0d1d5" : "#343746" }
      RowLayout {
        Layout.fillWidth: true
        spacing: 20
        ColumnLayout {
          Layout.preferredWidth: 225
          Label { text: "AT POPUP SIZE"; font.pixelSize: 10; font.letterSpacing: 1 }
          Text {
            Layout.fillWidth: true
            text: window.hoverHighlight ? "128 px · theme-aware\nHover to highlight each sample."
              : "128 px · theme-aware\nHighlighted / Dim to compare."
            textFormat: Text.PlainText
            color: window.muted
            font.pixelSize: 12
            lineHeight: 1.5
          }
        }
        Repeater {
          model: [{ state: "", label: "OUT OF CASE" }, { state: "open", label: "CASE OPEN" }, { state: "closed", label: "CASE CLOSED" }]
          ColumnLayout {
            id: sample
            required property var modelData
            Layout.fillWidth: true
            spacing: 0
            Pods.DotPods {
              Layout.alignment: Qt.AlignHCenter
              Layout.preferredWidth: 128
              Layout.preferredHeight: 128
              foregroundColor: window.foreground
              highlighted: window.hoverHighlight ? sampleHover.hovered : window.highlighted
              backgroundColor: window.background
              shadowColor: window.shade
              caseState: sample.modelData.state
              animating: false
              HoverHandler { id: sampleHover }
            }
            Label {
              Layout.alignment: Qt.AlignHCenter
              text: sample.modelData.label
              font.pixelSize: 9
              font.letterSpacing: 1
            }
          }
        }
      }
    }
  }

  // Capture the same component offscreen for review, without changing the panel.
  Timer {
    interval: 1000
    running: Qt.application.arguments.indexOf("--capture") >= 0
    onTriggered: {
      window.moving = false
      content.grabToImage(function(result) {
        result.saveToFile(window.lightTheme ? "/tmp/fleetpods-dotmesh-light.png" : "/tmp/fleetpods-dotmesh.png")
        Qt.quit()
      })
    }
  }
}
