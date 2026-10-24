import QtQuick
import "DotMesh.js" as Mesh
import "DotGrid.js" as Grid
import "artwork/AirPodsMesh.js" as AirPods

// Experimental popup artwork. Animation stops when hidden or explicitly paused.
Item {
  id: root
  property color backgroundColor: "#1c1d26"
  property color foregroundColor: "#e6e5df"
  property color shadowColor: "#465675"
  readonly property bool lightBackground: backgroundColor.hslLightness > 0.5
  property bool highlighted: true
  // Keep the theme hue; give the lit dots the same lift as the selected device.
  readonly property color litForeground: lightBackground ? foregroundColor : Qt.lighter(foregroundColor, 1.16)
  property string caseState: ""
  property bool animating: true
  // The popup supplies an already animated pose that survives image switches.
  // The standalone study animates its own state changes.
  property bool animatePodLifts: true
  property real dotPitch: Grid.pitch
  property real dotScale: Grid.scale
  property real zoom: 1
  property bool smoothSampling: false
  property bool alignDotsToPixels: true
  readonly property real pixelRatio: Math.max(1, Window.window ? Window.window.devicePixelRatio : Screen.devicePixelRatio)
  readonly property bool softDotEdges: alignDotsToPixels && pixelRatio === 1
  readonly property real effectiveDotPitch: alignDotsToPixels ? Grid.screenPitch(dotPitch, pixelRatio) : Math.max(1.5, dotPitch)
  property real yaw: -0.32
  property real pitch: 0.24
  property real time: 0
  // One progress value synchronizes the standalone descent and Y-axis turn.
  property real budsArrival: 1
  // Return pose: 1 faces the viewer, 0 is aligned with the charging well.
  property real leftTurn: 0
  property real rightTurn: 0
  property real lid: caseState === "closed" ? 0 : 1
  // 0 = docked, 1 = clear of the artwork. Keep the departing mesh until its
  // full travel finishes, instead of removing it when caseState changes.
  property real rightLift: caseState === "open-left" || caseState === "open-empty" ? 1 : 0
  property real leftLift: caseState === "open-right" || caseState === "open-empty" ? 1 : 0
  readonly property real artworkTop: height * 0.17
  readonly property var geometry: AirPods.geometry
  readonly property bool animationRunning: ticker.running
  implicitWidth: 128
  implicitHeight: 128

  Behavior on lid {
    enabled: root.visible
    NumberAnimation { duration: 350; easing.type: Easing.InOutCubic }
  }
  Behavior on rightLift {
    id: liftBehavior
    enabled: root.visible && root.animatePodLifts
    NumberAnimation {
      duration: liftBehavior.targetValue === 1 ? 840 : 420
      easing.type: Easing.InOutCubic
    }
  }
  Behavior on leftLift {
    id: leftLiftBehavior
    enabled: root.visible && root.animatePodLifts
    NumberAnimation {
      duration: leftLiftBehavior.targetValue === 1 ? 840 : 420
      easing.type: Easing.InOutCubic
    }
  }
  onVisibleChanged: if (visible) canvas.requestPaint()
  onTimeChanged: canvas.requestPaint()
  onBudsArrivalChanged: canvas.requestPaint()
  onLeftTurnChanged: canvas.requestPaint()
  onRightTurnChanged: canvas.requestPaint()
  onLidChanged: canvas.requestPaint()
  onRightLiftChanged: canvas.requestPaint()
  onLeftLiftChanged: canvas.requestPaint()
  onCaseStateChanged: canvas.requestPaint()
  onYawChanged: canvas.requestPaint()
  onPitchChanged: canvas.requestPaint()
  onDotPitchChanged: canvas.requestPaint()
  onDotScaleChanged: canvas.requestPaint()
  onZoomChanged: canvas.requestPaint()
  onSmoothSamplingChanged: canvas.requestPaint()
  onAlignDotsToPixelsChanged: canvas.requestPaint()
  onPixelRatioChanged: canvas.requestPaint()
  onLitForegroundChanged: canvas.requestPaint()
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
    opacity: root.highlighted ? 1 : 0.62
    Behavior on opacity { NumberAnimation { duration: 180; easing.type: Easing.OutSine } }
    // Canvas already renders at the window's device pixel ratio. An extra 2x
    // texture followed by downsampling creates uneven dots at fractional DPI.
    width: Math.ceil(root.width)
    height: Math.ceil(root.height)
    transform: Scale {
      xScale: root.alignDotsToPixels ? Grid.canvasScale(canvas.width, root.pixelRatio) : 1
      yScale: root.alignDotsToPixels ? Grid.canvasScale(canvas.height, root.pixelRatio) : 1
    }
    smooth: !root.alignDotsToPixels
    // Only the 100% display needs partial edge coverage. Keep the grid itself
    // pixel-aligned so every dot has the same footprint, without beat patterns.
    antialiasing: root.softDotEdges
    onPaint: {
      if (!root.visible) return
      var ctx = getContext("2d")
      ctx.reset()
      var pitch = root.effectiveDotPitch
      var columns = Math.max(1, Math.floor(root.width / pitch))
      var rows = Math.max(1, Math.floor(root.height / pitch))
      var frame = Mesh.renderFrame(root.geometry, root.caseState, root.lid, root.time,
        root.yaw + Math.sin(root.time * 0.45) * 0.22, root.pitch, columns, rows, root.lightBackground, root.rightLift, root.zoom, root.leftLift, root.budsArrival, root.smoothSampling,
        { left: root.leftTurn, right: root.rightTurn })
      var values = frame.values
      var style = Mesh.dotStyle(frame.tonal, pitch, root.backgroundColor,
        root.litForeground, root.shadowColor)
      var size = Math.min(pitch * 0.85, style.size * root.dotScale)
      if (root.alignDotsToPixels) size = Grid.screenSize(size, pitch, root.pixelRatio)
      var bands = new Uint8Array(values.length)
      for (var i = 0; i < values.length; i++) bands[i] = style.level(values[i])
      for (var band = 1; band <= style.levels; band++) {
        var color = style.colors[band]
        ctx.fillStyle = Qt.rgba(color[0], color[1], color[2], 1)
        for (var i = 0; i < values.length; i++) {
          if (bands[i] !== band) continue
          var x = (i % columns + 0.5) * pitch
          var y = (Math.floor(i / columns) + 0.5) * pitch
          var left = x - size / 2, top = y - size / 2
          if (root.alignDotsToPixels) {
            left = Math.round(left * root.pixelRatio) / root.pixelRatio
            top = Math.round(top * root.pixelRatio) / root.pixelRatio
          }
          ctx.globalAlpha = frame.coverage ? frame.coverage[i] : 1
          ctx.fillRect(left, top, size, size)
        }
      }
    }
  }
}
