import QtQuick
import "DotMesh.js" as Mesh
import "artwork/AirPodsMesh.js" as AirPods

// Experimental popup artwork. Animation stops when hidden or explicitly paused.
Item {
  id: root
  property color backgroundColor: "#1c1d26"
  property color foregroundColor: "#e6e5df"
  property color shadowColor: "#465675"
  readonly property bool lightBackground: backgroundColor.hslLightness > 0.5
  property string caseState: ""
  property bool animating: true
  property real dotPitch: 3
  property real yaw: -0.32
  property real pitch: 0.24
  property real time: 0
  property real lid: caseState === "closed" ? 0 : 1
  readonly property real artworkTop: height * 0.17
  readonly property var geometry: AirPods.geometry
  readonly property bool animationRunning: ticker.running
  implicitWidth: 128
  implicitHeight: 128

  Behavior on lid { NumberAnimation { duration: 700; easing.type: Easing.InOutCubic } }
  onTimeChanged: canvas.requestPaint()
  onLidChanged: canvas.requestPaint()
  onCaseStateChanged: canvas.requestPaint()
  onYawChanged: canvas.requestPaint()
  onPitchChanged: canvas.requestPaint()
  onDotPitchChanged: canvas.requestPaint()
  onForegroundColorChanged: canvas.requestPaint()
  onShadowColorChanged: canvas.requestPaint()
  onBackgroundColorChanged: canvas.requestPaint()

  Timer {
    id: ticker
    interval: 50
    running: root.animating && root.visible
    repeat: true
    onTriggered: root.time += interval / 1000
  }

  Canvas {
    id: canvas
    anchors.fill: parent
    onPaint: {
      var ctx = getContext("2d")
      ctx.reset()
      var pitch = Math.max(1.5, root.dotPitch)
      var columns = Math.max(1, Math.floor(width / pitch))
      var rows = Math.max(1, Math.floor(height / pitch))
      var frame = Mesh.renderFrame(root.geometry, root.caseState, root.lid, root.time,
        root.yaw + Math.sin(root.time * 0.45) * 0.22, root.pitch, columns, rows, root.lightBackground)
      var values = frame.values
      var c = root.foregroundColor, shadow = root.shadowColor
      // A single palette covers the whole object in each theme. All light
      // surfaces use the same full lattice and fade into the actual background.
      var levels = frame.tonal ? 64 : 8
      var background = root.backgroundColor
      var size = frame.tonal ? Math.max(1.05, pitch * 0.48) : Math.max(0.85, pitch * 0.40)
      for (var band = 1; band <= levels; band++) {
        if (frame.tonal) {
          var opacity = Math.pow(band / levels, 0.75)
          ctx.fillStyle = Qt.rgba(background.r + (c.r - background.r) * opacity,
            background.g + (c.g - background.g) * opacity,
            background.b + (c.b - background.b) * opacity, 1)
        } else {
          var mix = Math.pow(band / levels, 1.3)
          ctx.fillStyle = Qt.rgba(shadow.r + (c.r - shadow.r) * mix,
            shadow.g + (c.g - shadow.g) * mix, shadow.b + (c.b - shadow.b) * mix, 1)
        }
        for (var i = 0; i < values.length; i++) {
          var level = frame.tonal ? Math.round(values[i] * levels) : Math.ceil(values[i] * levels)
          if (level !== band) continue
          var x = (i % columns + 0.5) * pitch
          var y = (Math.floor(i / columns) + 0.5) * pitch
          ctx.fillRect(x - size / 2, y - size / 2, size, size)
        }
      }
    }
  }
}
