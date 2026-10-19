import QtQuick
import "../../plugin/DotMesh.js" as Mesh
import "../../plugin/DotGrid.js" as Grid
import "../../plugin/artwork/AirPodsMesh.js" as AirPods

// Run with the same Qt JS engine as the plugin:
// QT_FORCE_STDERR_LOGGING=1 QT_QPA_PLATFORM=offscreen qml tools/artwork/SamplingBenchmark.qml -- --scale=1.6
// Measures CPU mesh rendering only, excluding canvas painting and presentation.
Item {
  id: root
  property real displayScale: 2
  readonly property real dotPitch: Grid.screenPitch(3, displayScale)
  readonly property int cells: Math.round(Grid.podsFrameSize(128, dotPitch) / dotPitch)
  readonly property real zoom: (0.98 - (displayScale === 2 ? 0 : 2 / displayScale / 128)) * 1.38
  property int frame: 0
  property var timings: [[], [], [], []]
  readonly property int framesPerBlock: 24
  readonly property int blocks: 4 * 4 // first round warms up; three measured rounds

  Component.onCompleted: {
    for (var arg of Qt.application.arguments)
      if (arg.indexOf("--scale=") === 0) displayScale = Number(arg.slice(8))
    if (!(displayScale >= 1 && displayScale <= 4)) throw new Error("Expected --scale=1..4")
    ticker.start()
  }

  function report() {
    for (var mode = 0; mode < 4; ++mode) {
      var samples = timings[mode].slice().sort(function(a, b) { return a - b })
      var sum = samples.reduce(function(a, b) { return a + b }, 0)
      console.warn(JSON.stringify({theme: mode < 2 ? "dark" : "light", smooth: mode % 2 === 0,
        scale: displayScale, cells: cells, frames: samples.length,
        meanMs: sum / samples.length, medianMs: samples[Math.floor(samples.length / 2)],
        p95Ms: samples[Math.floor(samples.length * 0.95)]}))
    }
    Qt.quit()
  }

  Timer {
    id: ticker
    interval: 1; repeat: true
    onTriggered: {
      var block = Math.floor(root.frame / root.framesPerBlock)
      var mode = block % 4
      // Never benchmark a settled, cached pose. Replay the complete turn.
      var arrival = 0.02 + 0.96 * (root.frame % root.framesPerBlock) / (root.framesPerBlock - 1)
      var start = Date.now()
      var rendered = Mesh.renderFrame(AirPods.geometry, "", 1, 0, 0, 0.24,
        root.cells, root.cells, mode >= 2, 0, root.zoom, 0, arrival, mode % 2 === 0)
      var elapsed = Date.now() - start
      if (rendered.values.length !== root.cells * root.cells) throw new Error("Incomplete frame")
      if (block >= 4) root.timings[mode].push(elapsed)
      root.frame++
      if (root.frame === root.blocks * root.framesPerBlock) { stop(); root.report() }
    }
  }
}
