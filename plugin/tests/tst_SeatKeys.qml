import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "SeatKeys"
  when: windowShown
  width: 300; height: 100
  Pods.SeatKeyCatcher { id: catcher; anchors.fill: parent }
  SignalSpy { id: moves; target: catcher; signalName: "moveRequested" }
  SignalSpy { id: activations; target: catcher; signalName: "activateRequested" }

  function init() {
    catcher.blocked = false
    catcher.forceActiveFocus()
    moves.clear(); activations.clear()
  }

  function test_typingCannotNavigateOrActivateSeats() {
    for (var key of [Qt.Key_H, Qt.Key_J, Qt.Key_K, Qt.Key_L, Qt.Key_O, Qt.Key_A, Qt.Key_Space])
      keyClick(key)
    compare(moves.count, 0)
    compare(activations.count, 0)
  }

  function test_arrowsAndEnterRemainAvailable() {
    keyClick(Qt.Key_Right)
    keyClick(Qt.Key_Return)
    compare(moves.count, 1)
    compare(moves.signalArguments[0][0], 1)
    compare(activations.count, 1)
  }

  function test_pairingInputBlocksNavigation() {
    catcher.blocked = true
    keyClick(Qt.Key_Right)
    keyClick(Qt.Key_Return)
    compare(moves.count, 0)
    compare(activations.count, 0)
  }
}
