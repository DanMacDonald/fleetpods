import QtQuick
import QtQuick.Shapes

Item {
  id: root
  property int level: -1
  property string label: ""
  property color foreground: "#a9b1d6"
  property color lowColor: "#f7768e"
  // Fills the unfilled band in light themes: the status log's near-white tint.
  property color emptyFill: Qt.alpha(foreground, 0.035)
  // Keep the green visible even when a light theme uses a black foreground.
  property color healthyColor: Qt.hsla(0.4, 0.6, Math.max(0.4, Math.min(0.8, foreground.hslLightness)), 1)
  property string fontFamily: ""
  property string unknownCaption: ""
  property real ringSize: 28
  readonly property int lowBatteryPercent: 10
  readonly property bool known: level >= 0
  readonly property color fillColor: known && level < lowBatteryPercent ? lowColor : healthyColor
  readonly property real fraction: known ? Math.min(100, level) / 100 : 0
  readonly property real bandWidth: 2.5
  readonly property real bandRadius: (ringSize - 3) / 2
  // Light themes fill the empty part with a near-white tint; dark themes keep
  // a faint track under the charge. A light theme has a dark foreground.
  readonly property bool tintedTrack: foreground.hslLightness < 0.5
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
      // Dark themes: a faint filled track under the charge
      strokeColor: root.tintedTrack ? "transparent" : Qt.alpha(root.foreground, 0.25)
      strokeWidth: root.bandWidth
      fillColor: "transparent"
      PathAngleArc {
        centerX: root.ringSize / 2; centerY: root.ringSize / 2
        radiusX: root.bandRadius; radiusY: radiusX
        startAngle: -90; sweepAngle: 360
      }
    }
    // Light themes: the empty part of the band gets a near-white fill,
    // covering only the arc the charge does not.
    ShapePath {
      strokeColor: root.tintedTrack ? root.emptyFill : "transparent"
      strokeWidth: root.bandWidth
      fillColor: "transparent"
      capStyle: ShapePath.FlatCap
      PathAngleArc {
        centerX: root.ringSize / 2; centerY: root.ringSize / 2
        radiusX: root.bandRadius; radiusY: radiusX
        startAngle: -90 + 360 * root.fraction; sweepAngle: 360 * (1 - root.fraction)
      }
    }
    ShapePath {
      strokeColor: root.fillColor
      strokeWidth: root.bandWidth
      fillColor: "transparent"
      capStyle: ShapePath.FlatCap
      PathAngleArc {
        centerX: root.ringSize / 2; centerY: root.ringSize / 2
        radiusX: root.bandRadius; radiusY: radiusX
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
