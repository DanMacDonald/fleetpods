// Run: QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input plugin/tests -import plugin/tests/imports
import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "DeviceGlow"
  when: windowShown
  visible: true
  width: 120
  height: 120
  readonly property int fadeMs: 400
  readonly property int holdMs: 2000

  Component { id: icon; Pods.DeviceIcon { lineColor: "white"; screenFill: "black" } }

  // 2026-10-07: the breathing animation was dropped; the glow holds steady while the device plays.
  function test_playingShowsASteadyGlow() {
    var item = createTemporaryObject(icon, this, { selected: true, playing: true })
    var halo = findChild(item, "halo")
    var notes = findChild(item, "musicNotes")
    verify(notes !== null, "playing devices emit music notes")
    tryCompare(halo, "opacity", 1, fadeMs * 2)
    tryCompare(notes, "opacity", 1, fadeMs * 2)
    var lowest = 1
    for (var elapsed = 0; elapsed < holdMs; elapsed += 40) { wait(40); lowest = Math.min(lowest, halo.opacity) }
    compare(lowest, 1)
    item.playing = false
    tryCompare(halo, "opacity", 0, fadeMs * 2)
    tryCompare(notes, "opacity", 0, fadeMs * 2)
  }

  function test_notesRestartAtIcon_data() {
    return [{ tag: "resume playback", property: "playing" },
      { tag: "reopen panel", property: "visible" }]
  }

  function test_notesRestartAtIcon(data) {
    var item = createTemporaryObject(icon, this, { selected: true, playing: true })
    var streams = findChild(item, "musicStreams")
    wait(1100)
    verify(streams.itemAt(0).progress > 0.3)
    item[data.property] = false
    wait(fadeMs + 100)
    item[data.property] = true
    wait(30)
    for (var i = 0; i < streams.count; ++i)
      verify(streams.itemAt(i).progress < 0.1, "note " + i + " must start behind the icon")
    wait(250)
    verify(streams.itemAt(0).progress > 0.1, "fresh notes rise after restarting")
  }

  // A paused owner shows no glow; the glow means playback.
  function test_pausedDeviceHasNoGlow() {
    var item = createTemporaryObject(icon, this, { selected: true, playing: false })
    wait(fadeMs + 200)
    compare(findChild(item, "halo").opacity, 0)
    compare(findChild(item, "musicNotes").opacity, 0)
  }
}
