// Run: QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input plugin/tests -import plugin/tests/imports
import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "AirPodsIconSeats"
  when: windowShown
  width: 200
  height: 200
  readonly property int settledMs: 500
  readonly property int interruptedMs: 100

  Component { id: icon; Pods.AirPodsIcon { iconSize: 24; variant: "pro" } }

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
