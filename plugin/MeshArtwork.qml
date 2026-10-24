import QtQuick
import Qt5Compat.GraphicalEffects
import "DotGrid.js" as Grid

// The framed artwork shared by the popup and previews. Callers supply a pose;
// this module owns the camera, physical grid, palette and connector anchors.
Item {
  id: root
  required property color foregroundColor
  required property color backgroundColor
  property bool highlighted: true
  property string caseState: ""
  property real leftLift: caseState === "open-right" || caseState === "open-empty" ? 1 : 0
  property real rightLift: caseState === "open-left" || caseState === "open-empty" ? 1 : 0
  property real leftTurn: 0
  property real rightTurn: 0
  property real arrival: 1
  property bool pairingMode: false
  property real dotPitch: Grid.pitch
  property real artworkScale: 1
  readonly property bool showsCase: caseState !== ""
  readonly property real artworkTop: showsCase
    ? mesh.y + mesh.height * (0.23 - 0.07 * mesh.lid)
    : (height - width) / 2 + restingPodsTop(width)

  function podsFrameSize(size) {
    return Math.floor(size / 0.90 / mesh.effectiveDotPitch) * mesh.effectiveDotPitch
  }

  // Available even while showing the case, for its outgoing connector reveal.
  function restingPodsTop(size) {
    return size / 2 - podsFrameSize(size) * 0.22 * artworkScale
  }

  DotPods {
    id: mesh
    objectName: root.showsCase ? "caseMesh" : "budsMesh"
    // Frame complete cells. The case fills 64% of this camera's viewport.
    width: root.showsCase
      ? Math.floor(root.width / 0.64 / effectiveDotPitch) * effectiveDotPitch
      : root.podsFrameSize(root.width)
    height: width
    // Standalone tips need a whole physical-pixel origin at 100% to retain
    // the complete rightmost light-theme glyph. Preserve the case baseline.
    x: root.showsCase ? (root.width - width) / 2
      : Math.floor((root.width - width) * pixelRatio / 2) / pixelRatio
    y: root.showsCase
      ? Math.round(((root.height + root.width * 416 / 564) / 2 - height * 0.71) * 2) / 2
      : (root.height - height) / 2
    caseState: root.caseState
    leftLift: root.leftLift
    rightLift: root.rightLift
    leftTurn: root.leftTurn
    rightTurn: root.rightTurn
    budsArrival: root.arrival
    animatePodLifts: false
    foregroundColor: root.foregroundColor
    highlighted: root.highlighted
    backgroundColor: root.backgroundColor
    shadowColor: Qt.tint(root.backgroundColor, Qt.rgba(root.foregroundColor.r,
      root.foregroundColor.g, root.foregroundColor.b, 0.08))
    dotPitch: root.dotPitch
    // Keep one camera at every display scale. Shrinking the fractional-scale
    // frame puts shallow stem notches on a dot boundary and creates dark divots.
    zoom: 0.98 * (root.showsCase ? 1 : root.artworkScale)
    smoothSampling: !root.showsCase && root.arrival >= 1
    yaw: 0
    pitch: 0.24
    animating: false
  }

  Item {
    id: pairingLight
    objectName: "pairingLight"
    visible: root.pairingMode && root.showsCase && root.caseState !== "closed"
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
