import QtQuick
import Qt5Compat.GraphicalEffects
import "DotGrid.js" as Grid

// Popup framing for the study renderer. A fixed view keeps the case body and
// the battery/connection layout still while the hinge opens or closes.
Item {
  id: root
  required property color foregroundColor
  required property color backgroundColor
  property string caseState: "closed"
  property real leftLift: caseState === "open-right" || caseState === "open-empty" ? 1 : 0
  property real rightLift: caseState === "open-left" || caseState === "open-empty" ? 1 : 0
  property real leftTurn: 0
  property real rightTurn: 0
  property bool pairingMode: false
  property real dotPitch: Grid.pitch
  readonly property real artworkTop: mesh.y + mesh.height * (0.23 - 0.07 * mesh.lid)

  DotPods {
    id: mesh
    objectName: "caseMesh"
    // At this camera angle the mesh occupies 64% of its square viewport.
    // Match the image artwork's full width and fixed case-bottom baseline.
    width: Math.floor(root.width / 0.64 / root.dotPitch) * root.dotPitch
    height: width
    anchors.horizontalCenter: parent.horizontalCenter
    anchors.alignWhenCentered: false
    y: Math.round(((root.height + root.width * 416 / 564) / 2 - height * 0.71) * 2) / 2
    caseState: root.caseState
    leftLift: root.leftLift
    rightLift: root.rightLift
    leftTurn: root.leftTurn
    rightTurn: root.rightTurn
    animatePodLifts: false
    foregroundColor: root.foregroundColor
    backgroundColor: root.backgroundColor
    shadowColor: Qt.tint(root.backgroundColor, Qt.rgba(root.foregroundColor.r,
      root.foregroundColor.g, root.foregroundColor.b, 0.08))
    dotPitch: root.dotPitch
    // Fit complete edge glyphs inside the icon's clip, not just their centers.
    // Inset geometry only: the fixed grid and every dot's size stay unchanged.
    zoom: 0.98
    yaw: 0
    pitch: 0.24
    animating: false
  }

  Item {
    id: pairingLight
    objectName: "pairingLight"
    visible: root.pairingMode && root.caseState !== "closed"
    width: 2 * Math.max(2, root.width * 0.022)
    height: width
    anchors.horizontalCenter: parent.horizontalCenter
    y: mesh.y + mesh.height * 0.48 - height / 2
    RadialGradient {
      anchors.fill: parent
      gradient: Gradient {
        GradientStop { position: 0.0; color: "#ffffff" }
        GradientStop { position: 0.25; color: "#f2ffffff" }
        GradientStop { position: 0.5; color: "#80ffffff" }
        GradientStop { position: 0.75; color: "#26ffffff" }
        GradientStop { position: 1.0; color: "#00ffffff" }
      }
    }
    SequentialAnimation on opacity {
      running: pairingLight.visible && root.visible
      loops: Animation.Infinite
      NumberAnimation { from: 0; to: 1; duration: 900; easing.type: Easing.InOutSine }
      NumberAnimation { from: 1; to: 0; duration: 900; easing.type: Easing.InOutSine }
    }
  }
}
