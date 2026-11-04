import QtQuick
import "artwork/AirPodsMaxMesh.js" as Asset
import "rendering/Max.js" as Scene

// Reuse the actual panel painter, including fractional-DPI alignment,
// coverage, palette quantization and foreground highlighting.
DotPods {
  id: root
  property real rest: 0
  property bool animatePose: true
  // Returning on-head uses the full duration; closing off-head is twice as fast.
  property int poseDuration: 240
  // Level the cup bottoms in the approved rear three-quarter view.
  property real roll: -0.133
  artworkTop: height * 0.10
  animating: false
  smoothSampling: true
  yaw: -2.6
  pitch: 0.24
  // Centre the strap under the fixed connector pair. Whole dot columns keep
  // the artwork aligned to physical pixels at every display scale.
  transform: Translate { x: 2 * root.effectiveDotPitch }
  shadowColor: Qt.tint(backgroundColor, Qt.rgba(foregroundColor.r,
    foregroundColor.g, foregroundColor.b, 0.08))
  onRestChanged: requestPaint()
  onRollChanged: requestPaint()
  Behavior on rest {
    id: poseChange
    enabled: root.visible && root.animatePose
    NumberAnimation {
      duration: poseChange.targetValue === 1 ? root.poseDuration / 2 : root.poseDuration
      easing.type: Easing.OutCubic
    }
  }
  function renderFrame(columns, rows) {
    return Scene.frame(Asset.geometry, rest, yaw, pitch, columns, rows, lightBackground, roll)
  }
}
