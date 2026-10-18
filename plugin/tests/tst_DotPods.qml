import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "DotPods"
  when: windowShown
  visible: true
  width: 160
  height: 160
  Component { id: artwork; Pods.DotPods { width: 128; height: 128 } }

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
