import QtQuick
import QtQuick.Shapes

Item {
  id: root
  property int level: -1
  property string label: ""
  property color foreground: "#a9b1d6"
  property color lowColor: "#f7768e"
  // Keep the green visible even when a light theme uses a black foreground.
  property color healthyColor: Qt.hsla(0.4, 0.6, Math.max(0.4, Math.min(0.8, foreground.hslLightness)), 1)
  property string fontFamily: ""
  property string unknownCaption: ""
  property real ringSize: 28
  readonly property int lowBatteryPercent: 10
  readonly property bool known: level >= 0
  readonly property color fillColor: known && level < lowBatteryPercent ? lowColor : healthyColor
  readonly property real fraction: known ? Math.min(100, level) / 100 : 0
  readonly property string caption: !known && unknownCaption ? unknownCaption
    : (label ? label + " " : "") + (known ? level + "%" : "--")
  implicitWidth: Math.max(ringSize, captionText.implicitWidth)
  implicitHeight: ringSize + 6 + captionText.implicitHeight
  Accessible.role: Accessible.Indicator
  Accessible.name: label + " battery"
  Accessible.description: known ? level + "% remaining" : unknownCaption || "Battery level unavailable"

  Shape {
    width: root.ringSize
    height: width
    anchors.horizontalCenter: parent.horizontalCenter
    preferredRendererType: Shape.CurveRenderer
    ShapePath {
      strokeColor: Qt.alpha(root.foreground, 0.25)
      strokeWidth: 2.5
      fillColor: "transparent"
      PathAngleArc {
        centerX: root.ringSize / 2; centerY: root.ringSize / 2
        radiusX: (root.ringSize - 3) / 2; radiusY: radiusX
        startAngle: -90; sweepAngle: 360
      }
    }
    ShapePath {
      strokeColor: root.fillColor
      strokeWidth: 2.5
      fillColor: "transparent"
      capStyle: ShapePath.FlatCap
      PathAngleArc {
        centerX: root.ringSize / 2; centerY: root.ringSize / 2
        radiusX: (root.ringSize - 3) / 2; radiusY: radiusX
        startAngle: -90; sweepAngle: 360 * root.fraction
      }
    }
  }
  Text {
    id: captionText
    anchors.top: parent.top
    anchors.topMargin: root.ringSize + 6
    anchors.horizontalCenter: parent.horizontalCenter
    text: root.caption
    textFormat: Text.PlainText
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: 10
  }
}
