import QtQuick
import QtQuick.Shapes

Item {
  id: root
  required property color ink
  property bool paired: false
  implicitWidth: 24
  implicitHeight: 24
  layer.enabled: true
  opacity: ink.a

  Item {
    anchors.centerIn: parent
    width: 24
    height: 24
    scale: root.width / width
    ShipSilhouette {
      visible: root.paired
      x: 7
      y: -1
      scale: 0.72
      transformOrigin: Item.TopLeft
      opacity: 0.45
      ink: root.ink
    }
    ShipSilhouette {
      x: root.paired ? -1 : 0
      y: root.paired ? 4 : 0
      scale: root.paired ? 0.82 : 1
      transformOrigin: Item.TopLeft
      ink: root.ink
    }
  }

  component ShipSilhouette: Shape {
    required property color ink
    width: 24
    height: 24
    ShapePath {
      strokeWidth: -1
      fillColor: Qt.rgba(ink.r, ink.g, ink.b, 1)
      PathSvg { path: "M 11 1 H 13 V 5 H 18 V 10 L 22 12 L 19 19 L 12 23 L 5 19 L 2 12 L 6 10 V 5 H 11 Z" }
    }
  }
}
