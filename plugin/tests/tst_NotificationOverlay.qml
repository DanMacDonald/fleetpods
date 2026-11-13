import QtQuick
import QtTest
import ".." as Pods
import "../rendering/Raster.js" as Raster

TestCase {
  id: test
  name: "NotificationOverlay"
  when: windowShown
  visible: true
  width: 400
  height: 80
  property int destroyedOverlays: 0
  Component {
    id: cardComponent
    Rectangle {
      width: 400; height: 80; color: "#ff00ff"
      property real contentOpacity: 1
      property bool redacted: false
      property real textOffset: 0
      property real textBlock: 40
      property alias slot: slot
      Item { id: slot; x: 12; y: 20; width: 40; height: 40 }
    }
  }
  Component {
    id: controllerComponent
    QtObject {
      property string phase: "playing"
      property real artworkProgress: 0
      property string variant: "max"
      property var input: ({name: "AirPods Max 2", battery: 92})
      readonly property var presentation: ({phase: phase, artworkProgress: artworkProgress,
        variant: variant, name: input.name, battery: input.battery, text: {detail: phase}})
    }
  }
  Component {
    id: overlayComponent
    Pods.NotificationOverlay { Component.onDestruction: test.destroyedOverlays++ }
  }

  function test_singleHalfTurnContinuesThroughPlaying_data() {
    return [{tag: "max", variant: "max"}, {tag: "pods", variant: "pro"}]
  }
  function test_singleHalfTurnContinuesThroughPlaying(data) {
    var card = createTemporaryObject(cardComponent, test)
    var controller = createTemporaryObject(controllerComponent, test,
      {phase: "routing", variant: data.variant})
    var cover = createTemporaryObject(overlayComponent, card,
      {source: controller, width: card.width, height: card.height,
        contentOpacity: Qt.binding(function() { return card.contentOpacity })})
    var artwork = findChild(cover, "notificationDotArtwork")
    compare(artwork.yaw, data.variant === "max" ? Math.PI : 0,
      "Max starts at the rear; buds keep a fixed frontal camera")
    var initialParts = data.variant === "pro" ? artwork.budParts() : []
    // Check the entire turn against the progress ring's inner radius, not
    // merely the square artwork bounds (the cups reach into its corners).
    for (var angle = 0; angle <= 12; angle++) {
      controller.artworkProgress = angle / 12
      if (data.variant === "pro") {
        var parts = artwork.budParts()
        compare(artwork.yaw, 0, "the pair does not orbit")
        for (var part = 0; part < parts.length; part++) {
          verify(parts[part].at[0] * initialParts[part].at[0] > 0, "buds keep their sides")
          verify(Math.abs(parts[part].at[0]) >= Math.abs(initialParts[part].at[0]),
            "buds separate as the tips turn inward")
          verify(parts[part].at[2] <= 0, "buds retreat into the ring")
          verify(Math.abs(parts[part].at[1] * Math.cos(artwork.pitch)
            - parts[part].at[2] * Math.sin(artwork.pitch)) < 0.000001,
            "retreat keeps the pair centered vertically")
          compare(parts[part].angle[0], initialParts[part].angle[0])
          compare(parts[part].angle[2], initialParts[part].angle[2])
          compare(parts[part].mirror, initialParts[part].mirror)
          var side = parts[part].at[0] < 0 ? -1 : 1
          var expected = -side * (1.05 * (1 - artwork.turnProgress)
            + Math.PI * artwork.turnProgress)
          verify(Math.abs(parts[part].angle[1] - expected) < 0.000001,
            "buds start in the display pose and turn their tips toward each other")
        }
      }
      var pitch = artwork.effectiveDotPitch
      var columns = Math.floor(artwork.width / pitch)
      var frame = artwork.renderFrame(columns, columns)
      if (data.variant === "pro" && angle === 12) {
        var leftEdge = -Infinity, rightEdge = Infinity
        var view = {columns: columns, rows: columns, yaw: artwork.yaw, pitch: artwork.pitch,
          scale: columns / 2.95 * 1.3}
        for (var part = 0; part < parts.length; part++) {
          var points = Raster.projection(parts[part], view, function() { return 1 }).points
          for (var point = 0; point < points.length; point++) {
            if (parts[part].at[0] < 0) leftEdge = Math.max(leftEdge, points[point][0])
            else rightEdge = Math.min(rightEdge, points[point][0])
          }
        }
        verify(rightEdge - leftEdge > 1, "inward tips leave more than one dot pitch of clearance")
      }
      for (var i = 0; i < frame.values.length; i++) {
        if (!(frame.coverage ? frame.coverage[i] > 0 : frame.values[i] > 0)) continue
        var x = artwork.x + (i % columns + 0.5) * pitch - artwork.parent.width / 2
        var y = artwork.y + (Math.floor(i / columns) + 0.5) * pitch - artwork.parent.height / 2
        verify(Math.hypot(x, y) + pitch / Math.sqrt(2) < 17.2,
          "every painted dot clears the ring at turn step " + angle)
      }
    }
    controller.artworkProgress = 0.5
    var midpoint = artwork.turnProgress
    compare(midpoint, 0.5, "halfway through the turn at the midpoint")
    controller.phase = "playing"
    compare(artwork.turnProgress, midpoint, "playing does not cut the turn short")
    controller.artworkProgress = 0.25
    var earlyTurn = artwork.turnProgress
    verify(earlyTurn > 0 && earlyTurn < 0.125, "starts gently before accelerating")
    controller.artworkProgress = 0.75
    verify(Math.abs((1 - artwork.turnProgress) - earlyTurn) < 0.000001,
      "decelerates symmetrically toward the final pose")
    var previousTurn = 0
    for (var step = 0; step <= 20; step++) {
      controller.artworkProgress = step / 20
      verify(artwork.turnProgress >= previousTurn && artwork.turnProgress <= 1,
        "cubic easing approaches the front without overshooting or rebounding")
      previousTurn = artwork.turnProgress
    }
    controller.artworkProgress = 1
    compare(artwork.turnProgress, 1, "ends facing the viewer")
    controller.artworkProgress = 1.25
    controller.phase = "routing"
    wait(100)
    compare(artwork.turnProgress, 1, "holds the final pose even if connection is still pending")
    cover.retire()
    controller.artworkProgress = 0
    compare(artwork.turnProgress, 1, "dismissal retains the frontal pose")
  }

  function test_retirementKeepsCompletionCoveredUntilCardIsDestroyed() {
    var card = createTemporaryObject(cardComponent, test)
    var controller = createTemporaryObject(controllerComponent, test)
    var cover = createTemporaryObject(overlayComponent, card,
      {source: controller, width: card.width, height: card.height,
        contentOpacity: Qt.binding(function() { return card.contentOpacity })})
    var destroyed = destroyedOverlays
    cover.retire()
    controller.phase = "connecting"
    controller.variant = "pro"
    controller.input = {name: "Other pods", battery: 10}
    // A native exit may take longer than the old independent 300 ms timer.
    wait(600)
    compare(destroyedOverlays, destroyed)
    compare(cover.presentation.phase, "playing")
    compare(cover.presentation.name, "AirPods Max 2")
    compare(cover.presentation.variant, "max")
    compare(cover.presentation.battery, 92)
    card.contentOpacity = 0
    waitForRendering(card)
    var image = grabImage(card)
    compare(image.red(200, 25), 26, "fading the content cannot expose the native placeholder")
    compare(image.green(200, 25), 27)
    compare(image.blue(200, 25), 38)
    card.destroy()
    tryCompare(test, "destroyedOverlays", destroyed + 1)
  }
}
