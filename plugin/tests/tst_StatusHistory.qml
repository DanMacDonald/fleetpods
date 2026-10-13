import QtQuick
import QtTest
import ".." as Pods

// The log's rules are tested in model.test.js and replay.test.js; this is the mirror into the console's model.
TestCase {
  name: "StatusHistoryMirror"
  when: windowShown

  Component { id: history; Pods.StatusHistory {} }

  function row(id, message, live) {
    return { id: id, at: 0, kind: "connection", message: message, live: live === true, detail: "" }
  }

  function lines(item) {
    var result = []
    for (var i = 0; i < item.entries.count; ++i) result.push(item.entries.get(i).message)
    return result
  }

  function test_appendsAndRewritesInPlace() {
    var item = createTemporaryObject(history, this)
    item.rows = [row(1, "Connecting to AirPods...", true)]
    compare(lines(item), ["Connecting to AirPods..."])
    item.rows = [row(1, "Connecting to AirPods... Connected!"), row(2, "Omarchy Fleet connection switched to lg-gram.")]
    compare(lines(item), ["Connecting to AirPods... Connected!", "Omarchy Fleet connection switched to lg-gram."])
    compare(item.entries.get(0).live, false)
  }

  function test_eventKindReachesTheConsole() {
    var item = createTemporaryObject(history, this)
    var connecting = row(1, "Connecting to AirPods...", true)
    connecting.event = { kind: "connection" }
    item.rows = [connecting, row(2, "No event")]
    compare(item.entries.get(0).eventKind, "connection")
    compare(item.entries.get(1).eventKind, "")
    var connected = row(1, "Connecting to AirPods... Connected!")
    connected.event = { kind: "connected", fromAttempt: true }
    item.rows = [connected, row(2, "No event")]
    compare(item.entries.get(0).eventKind, "connected")
  }

  function test_clearAndTrimDropRows() {
    var item = createTemporaryObject(history, this)
    item.rows = [row(1, "a"), row(2, "b"), row(3, "c")]
    item.rows = [row(2, "b"), row(3, "c"), row(4, "d")]
    compare(lines(item), ["b", "c", "d"])
    item.rows = []
    compare(lines(item), [])
    item.rows = [row(5, "Available, in case")]
    compare(lines(item), ["Available, in case"])
  }
}
