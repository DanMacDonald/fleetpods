import QtQuick
import QtTest
import ".." as Pods
import "../DotGrid.js" as Grid

TestCase {
  name: "MeshArtwork"
  when: windowShown
  visible: true
  width: 520
  height: 520

  Component {
    id: capture
    Rectangle {
      width: 520; height: 520
      color: "#ff00ff"
      property alias art: art
      Pods.MeshArtwork {
        id: art
        x: 64; y: 64; width: 128; height: 128
        artworkScale: 1.38
        foregroundColor: "#afb4d8"
        backgroundColor: "#191b27"
      }
    }
  }

  function test_completePaintedGlyphs_data() {
    var rows = []
    for (var state of ["", "closed", "open", "open-left", "open-right", "open-empty"])
      for (var light of [false, true])
        for (var pitch of state === "" ? [2, 3] : [3])
          rows.push({tag: (state || "pods") + (light ? " light " : " dark ") + pitch,
            state: state, light: light, pitch: pitch})
    return rows
  }

  // Capture the same framed artwork used by the popup and preview, deliberately
  // without the popup clip. Any cut-off glyph then appears outside the slot.
  // No reimplementation of the renderer's sampling or glyph positioning here.
  function test_completePaintedGlyphs(data) {
    var item = createTemporaryObject(capture, this)
    var art = item.art
    art.caseState = data.state
    art.dotPitch = data.pitch
    if (data.state !== "") { art.width = 137; art.height = 137 }
    if (data.light) {
      art.foregroundColor = "#343b4c"
      art.backgroundColor = "#efeee8"
    }
    var ratio = Math.max(1, Window.window ? Window.window.devicePixelRatio : Screen.devicePixelRatio)
    // The offscreen grab can be cropped to logical dimensions; the entire
    // tested slot and its margins fit even on the 200% display.
    for (var arrival of data.state === "" ? [0, 0.25, 0.5, 0.75, 1] : [1]) {
      art.arrival = arrival
      // The entrance's translation is supplied by the icon/preview. Test
      // visible rows at that pose, not offscreen tips at the resting origin.
      art.y = 64 - (1 - arrival) * art.height
      wait(400)
      var shot = grabImage(item), painted = 0
      var left = art.x * ratio, right = (art.x + art.width) * ratio
      var top = 64 * ratio, bottom = (64 + art.height) * ratio
      for (var y = Math.floor(top - 32 * ratio); y < Math.ceil(bottom + 32 * ratio); ++y) {
        for (var x = Math.floor((art.x - 32) * ratio); x < Math.ceil((art.x + art.width + 32) * ratio); ++x) {
          var background = shot.red(x, y) === 255 && shot.green(x, y) === 0 && shot.blue(x, y) === 255
          if (background) continue
          painted++
          if (y >= Math.floor(top) && y < Math.ceil(bottom))
            verify(x >= Math.floor(left) && x < Math.ceil(right),
              "complete glyph fits the slot at arrival " + arrival + ", pixel " + x + "," + y)
          if (data.state === "" && arrival === 1)
            verify(y >= Math.floor(top) && y < Math.ceil(bottom),
              "resting pods fit complete glyphs vertically at row " + y)
        }
      }
      if (arrival === 1) verify(painted > 100, "captured actual painted artwork")
    }
  }

  function test_stemEdgeHasNoIsolatedDarkNotch() {
    var item = createTemporaryObject(capture, this)
    item.color = "black"
    var art = item.art
    art.foregroundColor = "white"
    art.backgroundColor = "black"
    var mesh = findChild(art, "budsMesh")
    verify(mesh !== null)
    wait(400)
    var shot = grabImage(item), ratio = mesh.pixelRatio, pitch = mesh.effectiveDotPitch
    var columns = Math.floor(mesh.width / pitch)
    // Popup fixtures at 100%/200% (47 cells) and 160% (45 cells).
    // Sample the outer left stem edge, including the shallow model notch.
    verify(columns === 45 || columns === 47)
    var column = columns === 45 ? 10 : 11
    var firstRow = columns === 45 ? 27 : 28
    var size = Grid.screenSize(pitch * 0.4 * mesh.dotScale, pitch, ratio)
    var origin = mesh.mapToItem(item, 0, 0)
    var levels = []
    for (var row = firstRow; row <= firstRow + 6; row++) {
      var x = Math.round(origin.x * ratio) + Math.round(((column + 0.5) * pitch - size / 2) * ratio)
      var y = Math.round(origin.y * ratio) + Math.round(((row + 0.5) * pitch - size / 2) * ratio)
      var level = 0
      // Accommodate the fractional origin without crossing into another dot.
      for (var dy = -1; dy <= 1; dy++)
        for (var dx = -1; dx <= 1; dx++) level = Math.max(level, shot.red(x + dx, y + dy))
      levels.push(level)
    }
    var surrounding = Math.min(levels[0], levels[levels.length - 1])
    verify(surrounding > 80, "captured the illuminated stem edge")
    for (var level of levels)
      verify(level >= surrounding * 0.8,
        "stem edge must fade smoothly without a dark divot: " + levels.join(","))
  }

  function test_restingConnectorMatchesTheStandaloneArtwork() {
    var item = createTemporaryObject(capture, this)
    var anchor = item.art.artworkTop
    item.art.caseState = "open"
    item.art.width = 137; item.art.height = 137
    fuzzyCompare(item.art.restingPodsTop(128), anchor, 0.00001,
      "case exit targets the very same anchor the incoming artwork uses")
  }
}
