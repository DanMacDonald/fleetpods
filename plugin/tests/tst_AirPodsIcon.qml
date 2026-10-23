// Run: QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input plugin/tests -import plugin/tests/imports
import QtQuick
import QtTest
import ".." as Pods
import "../DotMesh.js" as Mesh
import "../rendering/Raster.js" as Raster
import "../Model.js" as Model
import qs.Commons as Commons

TestCase {
  name: "AirPodsIconSeats"
  when: windowShown
  visible: true
  width: 200
  height: 200
  readonly property int settledMs: 500
  readonly property int interruptedMs: 100

  Component { id: icon; Pods.AirPodsIcon { iconSize: 24; variant: "pro" } }
  Component { id: popup; Pods.AirPodsIcon { iconSize: 128; variant: "pro"; pixelArt: true } }
  Component { id: motionMarker; Rectangle { x: 2; y: 96; width: 3; height: 3; color: "#ff00ff" } }

  function test_standaloneDescentUsesConstantScreenVelocity_data() {
    return [{tag: "dark", background: "#1a1b26"}, {tag: "light", background: "#ffffff"}]
  }

  function test_standaloneDescentUsesConstantScreenVelocity(data) {
    var previous = Commons.Color.popups.background
    Commons.Color.popups.background = data.background
    try {
      var item = createTemporaryObject(popup, this, {caseState: "closed", iconSize: 64, clip: false})
      var motion = findChild(item, "artworkMotion")
      createTemporaryObject(motionMarker, motion)
      item.caseState = ""
      var points = []
      for (var step = 0; step < 3; step++) {
        wait(90)
        var image = grabImage(this), found = -1
        for (var y = 0; y < 110; y++) {
          if (image.red(3, y) > 240 && image.green(3, y) < 20 && image.blue(3, y) > 240) {
            found = y
            break
          }
        }
        verify(found >= 0, "the scene-graph marker stays visible during the drop")
        points.push({y: found, time: Date.now()})
      }
      for (var i = 1; i < points.length; i++) {
        var travel = points[i].y - points[i - 1].y
        var expected = 64 * (points[i].time - points[i - 1].time) / 494
        verify(Math.abs(travel - expected) < 6,
          "screen-space descent stays linear: moved " + travel + ", expected " + expected)
      }
      tryCompare(item, "budsIn", 1, 1200)
      compare(motion.y, 0, "translation finishes exactly at its resting baseline")
      item.caseState = "open-left"
      compare(motion.y, 0, "returning to the case cannot inherit the entrance offset")
    } finally {
      Commons.Color.popups.background = previous
    }
  }

  function test_placementOwnsTheEntireLastPodDeparture() {
    var item = createTemporaryObject(popup, this, { caseState: "open-left" })
    var mesh = findChild(item, "caseMesh")
    item.caseState = ""
    wait(100)
    compare(mesh.leftLift, 0, "brief contradictory placement cannot lift the last pod")
    tryVerify(function() { return mesh.leftLift > 0 }, 600)
    verify(item.usesMeshCase, "the case survives confirmation and departure")
    tryVerify(function() { return item.caseOut > 0 }, 1400)
    compare(mesh.leftLift, 1, "case exit waits for both pods to clear")
    tryCompare(item, "caseExiting", false, 600)
    verify(item.usesMeshBuds)
    tryCompare(item, "budsIn", 1, 1200)
  }

  function test_returnCancelsPendingEmptyPresentation_data() {
    return [{ tag: "confirmation", delay: 100 }, { tag: "lift", delay: 400 }]
  }
  function test_returnCancelsPendingEmptyPresentation(data) {
    var item = createTemporaryObject(popup, this, { caseState: "open-left" })
    item.caseState = ""
    wait(data.delay)
    item.caseState = "open-left"
    var mesh = findChild(item, "caseMesh")
    tryCompare(mesh, "leftLift", 0, 900)
    wait(1100)
    verify(item.usesMeshCase, "a cancelled hold must never unload the returning case")
    compare(mesh.rightLift, 1)
    compare(item.caseOut, 0)
  }

  function test_hiddenDepartureCancelsAllPresentationTimers() {
    var item = createTemporaryObject(popup, this, { caseState: "open-left" })
    item.caseState = ""
    wait(100)
    item.visible = false
    item.visible = true
    verify(item.usesMeshBuds)
    compare(item.budsIn, 1)
    wait(1200)
    verify(item.usesMeshBuds)
    compare(item.budsIn, 1, "reopening must not replay a stale pending departure")
  }

  function test_popupGridStaysConsistentAcrossStates() {
    for (var size of [128, 160]) {
      for (var state of ["", "closed", "open", "open-left", "open-right", "open-empty"]) {
        var item = createTemporaryObject(popup, this, { caseState: state, iconSize: size })
        var mesh = findChild(item, state === "" ? "budsMesh" : "caseMesh")
        verify(mesh !== null)
        compare(mesh.dotPitch, 3, "artwork size and state must not change physical dot spacing")
        compare(mesh.dotScale, 1.4, "every view must retain the approved pods glyph weight")
        verify(mesh.alignDotsToPixels)
        fuzzyCompare(mesh.width / mesh.effectiveDotPitch, Math.round(mesh.width / mesh.effectiveDotPitch), 0.00001,
          "frame complete physical grid cells")
      }
    }
  }

  function test_standaloneStemHasNoDisplayScaleInset() {
    var item = createTemporaryObject(popup, this, { caseState: "", iconSize: 128 })
    var mesh = findChild(item, "budsMesh")
    if (mesh.pixelRatio !== 1 && mesh.pixelRatio !== 2)
      skip("The identical logical grids are on 100% and 200% displays")
    var columns = Math.floor(mesh.width / mesh.effectiveDotPitch)
    compare(columns, 47)
    var frame = Mesh.renderFrame(mesh.geometry, "", 1, 0, mesh.yaw, mesh.pitch,
      columns, columns, false, 0, mesh.zoom, 0, 1, mesh.smoothSampling)
    // This strip was half-covered only at 100% because the legacy inset
    // shrank the geometry even after dots had regained their 1.5 px weight.
    for (var y = 30; y <= 33; y++) {
      var stem = y * columns + 11
      compare(frame.coverage[stem], 1, "the left stem must stay fully covered at row " + y)
      verify(frame.values[stem] > 0.58, "the stem remains a lit surface")
    }
  }

  function test_popupLidKeepsRendererAndReverses() {
    var item = createTemporaryObject(popup, this, { caseState: "closed" })
    var mesh = findChild(item, "caseMesh")
    verify(mesh !== null)
    compare(mesh.lid, 0)
    verify(!mesh.animationRunning)
    item.caseState = "open"
    compare(findChild(item, "caseMesh"), mesh)
    wait(200)
    verify(mesh.lid > 0 && mesh.lid < 1)
    item.caseState = "closed"
    compare(findChild(item, "caseMesh"), mesh)
    tryCompare(mesh, "lid", 0, 1000)
    item.caseState = "open"
    tryCompare(mesh, "lid", 1, 1000)
    compare(mesh.time, 0)
  }

  function test_popupPodsDropAndTurnThenReturn() {
    var item = createTemporaryObject(popup, this, { caseState: "open-empty" })
    item.caseState = ""
    verify(item.caseExiting)
    verify(item.usesMeshCase)
    var caseMesh = findChild(item, "caseMeshArtwork")
    var restingY = caseMesh.parent.y
    wait(100)
    verify(caseMesh.parent.y > restingY, "empty case slides down before pods enter")
    verify(item.caseOut > 0 && item.caseOut < 1)
    tryCompare(item, "caseExiting", false, 600)
    verify(item.usesMeshBuds)
    var buds = findChild(item, "budsMesh")
    verify(buds !== null)
    verify(item.budsIn < 0.2, "pods begin their entrance only after the case exits")
    var connectorTop = item.artworkTop
    verify(connectorTop > 0, "connector ends below the entrance clip")
    wait(200)
    verify(item.budsIn > 0 && item.budsIn < 1)
    compare(item.artworkTop, connectorTop, "pod descent must not pull connectors upward")
    compare(buds.budsArrival, item.budsIn)
    tryCompare(item, "budsIn", 1, 1200)
    compare(item.artworkTop, connectorTop, "connector keeps its resting endpoint throughout entrance")
    verify(!buds.animationRunning)
    item.caseState = "open-left"
    var mesh = findChild(item, "caseMesh")
    verify(mesh !== null)
    verify(mesh.leftLift > 0.70 && mesh.leftLift <= 0.72)
    tryCompare(mesh, "leftLift", 0, 1300)
    compare(mesh.rightLift, 1)
  }

  function test_returnInterruptsStandaloneEntrance() {
    var item = createTemporaryObject(popup, this, { caseState: "open-empty" })
    createTemporaryObject(motionMarker, findChild(item, "artworkMotion"))
    item.caseState = ""
    tryCompare(item, "caseExiting", false, 600)
    wait(160)
    verify(item.budsIn > 0 && item.budsIn < 1)
    item.caseState = "open-right"
    var mesh = findChild(item, "caseMesh")
    compare(item.budsIn, 1)
    tryCompare(mesh, "rightLift", 0, 1300)
    compare(mesh.leftLift, 1)
    compare(item.budsIn, 1)
    waitForRendering(item)
    var image = grabImage(item)
    verify(image.red(3, 96) > 240 && image.green(3, 96) < 20 && image.blue(3, 96) > 240,
      "interrupting the render-thread drop restores the case's screen-space baseline")
  }

  function test_caseExitWaitsForLastPodAndCanBeInterrupted() {
    var item = createTemporaryObject(popup, this, { caseState: "open-left" })
    item.caseState = "open-empty"
    wait(100)
    item.caseState = ""
    verify(item.caseExiting)
    wait(100)
    verify(item.leftSeat > 0, "last pod is still leaving")
    compare(item.caseOut, 0, "case stays still until the last pod clears")
    tryVerify(function() { return item.caseOut > 0 }, 1200)
    compare(item.leftSeat, 0)
    compare(item.rightSeat, 0)
    item.caseState = "open-right"
    verify(!item.caseExiting)
    compare(item.caseOut, 0, "returning pod restores the case to its baseline")
    var mesh = findChild(item, "caseMesh")
    tryCompare(mesh, "rightLift", 0, 1000)
    verify(item.usesMeshCase, "cancelled exit must not launch standalone pods later")
  }

  function test_standaloneStartupAndHiddenChangesStaySettled() {
    var item = createTemporaryObject(popup, this, { caseState: "" })
    compare(item.budsIn, 1)
    compare(findChild(item, "budsMesh").budsArrival, 1)
    item.caseState = "open-empty"
    item.visible = false
    item.caseState = ""
    item.visible = true
    compare(item.budsIn, 1)
    item.caseState = "open-empty"
    item.caseState = ""
    wait(100)
    item.visible = false
    compare(item.budsIn, 1)
    item.visible = true
    compare(findChild(item, "budsMesh").budsArrival, 1)
  }

  function test_standaloneSmoothingReturnsAtRest() {
    var item = createTemporaryObject(popup, this, { caseState: "" })
    var mesh = findChild(item, "budsMesh")
    compare(mesh.smoothSampling, true, "initial resting artwork keeps its finish")
    for (var arrival of [0, 0.25, 0.75, 0.99]) {
      item.budsIn = arrival
      compare(mesh.smoothSampling, false, "moving pods use a single sample")
    }
    item.budsIn = 1
    compare(mesh.smoothSampling, true, "final frame restores smooth shading")
    item.budsIn = 0.5
    item.visible = false
    item.visible = true
    compare(item.budsIn, 1)
    compare(mesh.smoothSampling, true, "reopened artwork is settled and smoothed")
    item.caseState = "open"
    compare(findChild(item, "caseMesh").smoothSampling, false, "case motion already uses one sample")
  }

  function liftCases() {
    return [
      { tag: "right", start: "open", out: "open-left", lift: "rightLift", other: "leftLift", otherValue: 0 },
      { tag: "left", start: "open", out: "open-right", lift: "leftLift", other: "rightLift", otherValue: 0 },
      { tag: "left leaves second", start: "open-left", out: "open-empty", lift: "leftLift", other: "rightLift", otherValue: 1 },
      { tag: "right leaves second", start: "open-right", out: "open-empty", lift: "rightLift", other: "leftLift", otherValue: 1 }
    ]
  }

  function test_podLiftsAndReturnsWithoutReplacingCase_data() { return liftCases() }
  function test_podLiftsAndReturnsWithoutReplacingCase(data) {
    var item = createTemporaryObject(popup, this, { caseState: data.start })
    var mesh = findChild(item, "caseMesh")
    compare(mesh[data.lift], 0)
    item.caseState = data.out
    compare(findChild(item, "caseMesh"), mesh)
    wait(150)
    verify(mesh[data.lift] > 0 && mesh[data.lift] < 1)
    compare(mesh[data.other], data.otherValue)
    compare(mesh.lid, 1)
    item.caseState = data.start
    tryCompare(mesh, data.lift, 0, 1300)
    item.caseState = data.out
    wait(450)
    verify(mesh[data.lift] > 0 && mesh[data.lift] < 1, "upward travel should still be animating after the old 420 ms duration")
    tryCompare(mesh, data.lift, 1, 700)
    item.caseState = data.start
    wait(150)
    verify(mesh[data.lift] > 0 && mesh[data.lift] < 1)
    tryCompare(mesh, data.lift, 0, 1300)
    compare(mesh[data.other], data.otherValue)
    compare(mesh.time, 0)
  }

  function test_podsReturnBeforeLidCloses_data() {
    return [{ tag: "right", state: "open-left" }, { tag: "left", state: "open-right" },
      { tag: "both", state: "open-empty" }, { tag: "standalone", state: "" }]
  }
  function test_podsReturnBeforeLidCloses(data) {
    var item = createTemporaryObject(popup, this, { caseState: data.state })
    var mesh = findChild(item, "caseMesh")
    wait(settledMs)
    item.caseState = "closed"
    if (mesh === null) mesh = findChild(item, "caseMesh")
    wait(150)
    compare(mesh.lid, 1)
    if (data.state !== "open-right") verify(mesh.rightLift > 0 && mesh.rightLift < 1)
    if (data.state !== "open-left") verify(mesh.leftLift > 0 && mesh.leftLift < 1)
    tryCompare(mesh, "rightLift", 0, 1300)
    tryCompare(mesh, "leftLift", 0, 1300)
    tryCompare(mesh, "lid", 0, 1300)
    compare(findChild(item, "caseMesh"), mesh)
  }

  function test_hiddenPodStateDoesNotReplayLift_data() { return liftCases() }
  function test_hiddenPodStateDoesNotReplayLift(data) {
    var item = createTemporaryObject(popup, this, { caseState: data.start })
    var mesh = findChild(item, "caseMesh")
    item.visible = false
    item.caseState = data.out
    compare(mesh[data.lift], 1)
    item.visible = true
    compare(mesh[data.lift], 1)
  }

  function test_leftPlacementEventsDriveLift() {
    var item = createTemporaryObject(popup, this, { caseState: "open" })
    var mesh = findChild(item, "caseMesh")
    var docked = Model.podFrom({ available: true, in_case: true })
    var removed = Model.podFrom({ available: true, in_case: false })
    item.caseState = Model.caseIconState(false, Model.LID_OPEN, removed, docked,
      item.caseState, false, 1000, 1001, 20000, true, false)
    compare(item.caseState, "open-right")
    wait(250)
    verify(mesh.leftLift > 0 && mesh.leftLift < 1)
    compare(mesh.rightLift, 0)
    var progress = mesh.leftLift
    var worn = Model.podFrom({ available: true, in_case: false, in_ear: true })
    item.caseState = Model.caseIconState(false, Model.LID_OPEN, worn, docked,
      item.caseState, true, 1000, 1002, 20000, true, true)
    compare(mesh.leftLift, progress, "the later in-ear event must not restart departure")
    tryCompare(mesh, "leftLift", 1, 900)
    item.caseState = Model.caseIconState(false, Model.LID_OPEN, docked, docked,
      item.caseState, true, 1000, 1003, 20000, true, false)
    compare(item.caseState, "open")
    tryCompare(mesh, "leftLift", 0, 1300)
    compare(findChild(item, "caseMesh"), mesh)
  }

  function test_returnAfterEmptyCaseHold_data() {
    return [
      { tag: "left", state: "open-left", lift: "leftLift", other: "rightLift", initial: "open-empty" },
      { tag: "right", state: "open-right", lift: "rightLift", other: "leftLift", initial: "open-empty" },
      { tag: "left after startup", state: "open-left", lift: "leftLift", other: "rightLift", initial: "" },
      { tag: "right after startup", state: "open-right", lift: "rightLift", other: "leftLift", initial: "" }
    ]
  }

  function test_returnAfterEmptyCaseHold(data) {
    var item = createTemporaryObject(popup, this, { caseState: data.initial })
    // The service replaces the empty case with the standalone pair after
    // its hold. Returning must still animate after that renderer was unloaded.
    item.caseState = ""
    tryCompare(item, "caseExiting", false, 600)
    verify(!item.usesMeshCase)
    item.caseState = data.state
    var mesh = findChild(item, "caseMesh")
    verify(mesh !== null)
    verify(mesh[data.lift] > 0.70 && mesh[data.lift] <= 0.72, "return starts above the clipping boundary")
    var turn = data.lift === "leftLift" ? "leftTurn" : "rightTurn"
    var otherTurn = data.other === "leftLift" ? "leftTurn" : "rightTurn"
    compare(mesh[turn], 1, "return starts facing the viewer")
    compare(mesh[otherTurn], 0, "the absent pod does not turn")
    wait(150)
    verify(mesh[data.lift] > 0 && mesh[data.lift] < 1)
    compare(mesh[data.other], 1)
    compare(mesh.lid, 1)
    tryCompare(mesh, data.lift, 0, 1300)
    compare(mesh[data.other], 1)
    compare(mesh[turn], 0, "seated pods keep their charging-well orientation")
  }

  function test_returnRotationSurvivesReversal() {
    var item = createTemporaryObject(popup, this, { caseState: "" })
    item.caseState = "open-right"
    var mesh = findChild(item, "caseMesh")
    tryVerify(function() { return mesh.rightTurn < 0.8 }, item.meshReturnMs)
    verify(mesh.rightTurn > 0 && mesh.rightTurn < 1)
    var turn = mesh.rightTurn
    item.caseState = "open-empty"
    compare(mesh.rightTurn, turn, "reversing a return must not snap its angle")
    wait(60)
    verify(mesh.rightTurn > turn)
    item.caseState = "open-right"
    tryCompare(mesh, "rightLift", 0, 1300)
    compare(mesh.rightTurn, 0)
    item.caseState = "open-empty"
    compare(mesh.rightTurn, 0, "a new departure keeps the original vertical lift")
  }

  function test_returnTurnsVisiblyThenSeatsStraight() {
    var item = createTemporaryObject(popup, this, { caseState: "", iconSize: 130 })
    item.caseState = "open"
    var mesh = findChild(item, "caseMesh")
    compare(mesh.leftTurn, 1)
    tryVerify(function() { return mesh.leftLift < 0.30 }, 900)
    verify(mesh.leftLift > 0.12, "pod is still above its half-in position")
    verify(mesh.leftTurn > 0 && mesh.leftTurn < 1, "pod turns while visible")
    verify(item.clip, "returning pods must enter through the fixed artwork boundary")
    compare(mesh.leftTurn, mesh.rightTurn)
    tryVerify(function() { return mesh.leftLift <= 0.12 }, 600)
    compare(mesh.leftTurn, 0, "rotation finishes before the final descent")
    verify(mesh.leftLift > 0 && item.leftSeat < 1, "the last part of the drop remains")
    tryCompare(mesh, "leftLift", 0, 600)
    compare(mesh.leftTurn, 0)
    compare(item.clip, true)
  }

  function test_returnEntersStemFirstThroughClip() {
    for (var size of [128, 137, 160]) {
      var item = createTemporaryObject(popup, this, { caseState: "open-empty", iconSize: size })
      var mesh = findChild(item, "caseMesh")
      verify(item.clip)
      for (var side of ["left", "right"]) {
        for (var lift of [item.meshLift(0, true), 0.65]) {
          var parts = Mesh.parts(mesh.geometry, "open", 1, 0,
            side === "right" ? lift : 1, side === "left" ? lift : 1,
            undefined, { left: item.returnTurn(lift), right: item.returnTurn(lift) })
          var visibleVertices = 0
          for (var part of parts) {
            if (!part.dockedPod) continue
            for (var v of part.mesh.vertices) {
              var a = part.angle
              var p = Raster.rotate(v[0] * part.mirror, v[1], v[2], a[0], a[1], a[2])
              p = Raster.rotate(p[0] + part.at[0], p[1] + part.at[1], p[2] + part.at[2],
                mesh.pitch, mesh.yaw, 0)
              var y = mesh.height / 2 - p[1] * mesh.height / 3.6 * mesh.zoom * 6 / (6 - p[2])
              var point = mesh.mapToItem(item, 0, y)
              if (lift > 0.7) {
                verify(point.y < -mesh.dotPitch / 2, "the whole pod, including its tip, starts above the clip")
              } else if (point.y >= 0) {
                visibleVertices++
                verify(v[1] < -0.3, "only the lower stem enters on the first visible part of the drop")
              }
            }
          }
          if (lift < 0.7) verify(visibleVertices > 0, "the stem enters without a long hidden delay")
        }
      }
    }
  }

  function test_hiddenReturnDoesNotReplay() {
    var item = createTemporaryObject(popup, this, { caseState: "" })
    item.visible = false
    item.caseState = "open-left"
    var mesh = findChild(item, "caseMesh")
    compare(mesh.leftLift, 0)
    compare(mesh.rightLift, 1)
    item.visible = true
    compare(mesh.leftLift, 0)
  }

  function test_barKeepsGlyphs() {
    var item = createTemporaryObject(icon, this, { caseState: "closed" })
    verify(!item.usesMeshCase)
    item.caseState = "open"
    verify(!item.usesMeshCase)
    compare(findChild(item, "caseMesh"), null)
  }

  function test_meshPairingLightAndHiddenState() {
    var item = createTemporaryObject(popup, this, { caseState: "open", pairingMode: true })
    var light = findChild(item, "pairingLight")
    verify(light.visible)
    item.pairingMode = false
    verify(!light.visible)
    item.pairingMode = true
    item.visible = false
    item.caseState = "closed"
    var mesh = findChild(item, "caseMesh")
    compare(mesh.lid, 0)
    item.visible = true
    verify(!light.visible)
    compare(mesh.lid, 0)
  }

  function test_initialState() {
    var item = createTemporaryObject(icon, this, { caseState: "open-left" })
    wait(settledMs)
    compare(item.leftSeat, 1)
    compare(item.rightSeat, 0)
  }

  function test_interruptedChange_data() {
    return [
      { tag: "left to right", first: "open-left", next: "open-right", left: 0, right: 1 },
      { tag: "right to left", first: "open-right", next: "open-left", left: 1, right: 0 },
      { tag: "both returned", first: "open-empty", next: "open", left: 1, right: 1 }
    ]
  }

  function test_interruptedChange(data) {
    var item = createTemporaryObject(icon, this, { caseState: "open" })
    item.caseState = data.first
    wait(interruptedMs)
    item.caseState = data.next
    wait(settledMs)
    compare(item.leftSeat, data.left)
    compare(item.rightSeat, data.right)
  }
}
