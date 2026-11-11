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
  // Face the viewer squarely while worn; retain the angled resting pose.
  // Follow the same animated wear progress so camera and cups move together.
  property real roll: 0.133 * rest
  artworkTop: height * 0.10
  animating: false
  smoothSampling: true
  // View the opposite side of the resting pose so wear changes turn only
  // about 31 degrees instead of carrying a cup across the camera.
  yaw: (Math.PI - 2.6) * rest
  pitch: 0.24
  // Ease right with the resting rotation so the headband stays centered
  // beneath the fixed connectors. The frontal pose needs no correction.
  transform: Translate {
    x: root.effectiveDotPitch * root.rest
    // Keep the enlarged headband below the fixed connection endpoints.
    y: 2 * root.effectiveDotPitch
  }
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
