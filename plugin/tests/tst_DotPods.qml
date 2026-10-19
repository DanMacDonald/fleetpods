import QtQuick
import QtTest
import ".." as Pods
import "../DotGrid.js" as Grid

TestCase {
  name: "DotPods"
  when: windowShown
  visible: true
  width: 160
  height: 160
  Component { id: artwork; Pods.DotPods { width: 128; height: 128 } }
  Component {
    id: gridCapture
    Rectangle {
      width: 160; height: 160; color: "black"
      property alias mesh: dots
      Pods.DotPods {
        id: dots
        x: 10.3; y: 10.2; width: 128; height: 128
        animating: false; caseState: "closed"; yaw: 0
        backgroundColor: "black"; foregroundColor: "white"; shadowColor: "white"
      }
    }
  }

  function test_screenGrid_data() {
    return [1, 1.25, 1.5, 1.6, 1.75, 2, 2.5, 3].map(function(ratio) {
      return { tag: String(ratio), ratio: ratio }
    })
  }
  function test_screenGrid(data) {
    var pitch = Grid.screenPitch(3, data.ratio)
    fuzzyCompare(pitch * data.ratio, Math.round(3 * data.ratio), 0.00001)
    for (var requested of [1.68, 2.016]) {
      var size = Grid.screenSize(requested, pitch, data.ratio)
      fuzzyCompare(size * data.ratio, Math.round(size * data.ratio), 0.00001)
      verify((pitch - size) * data.ratio >= 0.99999)
    }
    if (data.ratio === 2) {
      compare(pitch, 3)
      compare(Grid.screenSize(1.68, pitch, 2), 1.5)
      compare(Grid.screenSize(2.016, pitch, 2), 2)
    }
  }

  function test_physicalDotsHaveUniformFootprints_data() {
    return ["closed", "open-empty", ""].map(function(state) {
      return { tag: state || "pods", state: state }
    })
  }
  function test_physicalDotsHaveUniformFootprints(data) {
    var item = createTemporaryObject(gridCapture, this)
    item.mesh.caseState = data.state
    wait(400)
    var shot = grabImage(item), ratio = item.mesh.pixelRatio
    var pitch = item.mesh.effectiveDotPitch
    var expected = Math.round(Grid.screenSize(Math.max(0.85, pitch * 0.4) * 1.4, pitch, ratio) * ratio)
    var count = 0
    for (var y = 0; y < shot.height; ++y) {
      var start = -1, previous = -1
      for (var x = 0; x <= shot.width; ++x) {
        var filled = x < shot.width && shot.red(x, y) > 127
        if (x < shot.width) verify(shot.red(x, y) === 0 || shot.red(x, y) === 255,
          "dot coverage must not create alternating dim edges")
        if (filled && start < 0) {
          start = x
          if (previous >= 0) compare((start - previous) % Math.round(pitch * ratio), 0,
            "dot starts stay on a uniform physical grid")
          previous = start
        }
        if (!filled && start >= 0) {
          compare(x - start, expected, "every dot row has the same physical width")
          start = -1; count++
        }
      }
    }
    verify(count > 100, "captured the rendered grid")
  }

  function test_hiddenArtworkStopsRendering() {
    var item = createTemporaryObject(artwork, this)
    tryVerify(function() { return item.time > 0 }, 500)
    item.visible = false
    verify(!item.animationRunning)
    var stopped = item.time
    wait(150)
    compare(item.time, stopped)
    item.visible = true
    tryVerify(function() { return item.time > stopped }, 500)
    item.animating = false
    verify(!item.animationRunning)
  }

  function test_caseOpensAndClosesWhileMotionIsPaused() {
    var item = createTemporaryObject(artwork, this, { caseState: "closed", animating: false })
    compare(item.lid, 0)
    item.caseState = "open"
    wait(200)
    verify(item.lid > 0 && item.lid < 1)
    tryCompare(item, "lid", 1, 900)
    item.caseState = "closed"
    tryCompare(item, "lid", 0, 900)
    compare(item.time, 0)
  }
}
