import QtQuick
import "DotMesh.js" as Mesh
import "rendering/Max.js" as Max
import "artwork/AirPodsMaxMesh.js" as Headset

// The panel's meshes and dot painter, framed for the native toast's icon slot.
DotPods {
  id: root
  property string variant: "max"
  property real progress: 0
  animating: false
  animatePodLifts: false
  smoothSampling: true
  dotPitch: 1.5
  // Cubic ease-in/out: gentle endpoints and a smooth, symmetric middle.
  // The publisher's existing timeline still owns both holds and dismissal.
  readonly property real turnProgress: {
    if (progress <= 0) return 0
    if (progress >= 1) return 1
    if (progress < 0.5)
      return 4 * progress * progress * progress
    var remaining = 2 * (1 - progress)
    return 1 - remaining * remaining * remaining / 2
  }
  yaw: variant === "max" ? Math.PI * (1 + turnProgress) : 0
  pitch: variant === "max" ? 0 : -0.24
  shadowColor: Qt.tint(backgroundColor, Qt.rgba(foregroundColor.r,
    foregroundColor.g, foregroundColor.b, 0.08))

  function budParts() {
    // Start in the front display pose, then turn each tip inward. Open a gap
    // as they turn and retreat along the view direction to stay inside the ring.
    var scene = Mesh.parts(geometry, "", 1, 0, 1, 1, 1)
    var separation = 0.5 + 0.15 * turnProgress
    var distance = 0.3 * turnProgress
    for (var i = 0; i < scene.length; i++) {
      var side = scene[i].at[0] < 0 ? -1 : 1
      // The tip bore follows local X; stop at inward, before the panel's
      // extra half-radian entry angle carries it past the opposite bud.
      scene[i].angle[1] = -side * (1.05 + (Math.PI - 1.05) * turnProgress)
      scene[i].at = [side * separation,
        -Math.sin(pitch) * distance, -Math.cos(pitch) * distance]
    }
    return scene
  }

  function renderFrame(columns, rows) {
    if (variant === "max")
      return Max.frame(Headset.geometry, 0, yaw, pitch, columns, rows, lightBackground, 0)
    return Mesh.renderScene(budParts(), {
      state: "", lid: 1, yaw: yaw, pitch: pitch, columns: columns, rows: rows,
      scale: Math.min(columns, rows) / 2.95 * 1.3
    }, lightBackground, true)
  }
  onTurnProgressChanged: requestPaint()
  onVariantChanged: requestPaint()
}
