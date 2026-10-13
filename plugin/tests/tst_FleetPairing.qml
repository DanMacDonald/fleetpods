import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "FleetPairing"
  when: windowShown
  visible: true
  width: 440
  height: 400
  Component { id: pane; Pods.FleetPairing { width: 400 } }
  SignalSpy { id: submitSpy; signalName: "submit" }
  SignalSpy { id: cancelSpy; signalName: "cancel" }
  SignalSpy { id: dismissSpy; signalName: "dismiss" }

  function test_waitingForInvitationDoesNotClaimSetupIsFinishing() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "waiting_offer", peer_name: "Laptop A", initiator: false }
    })
    compare(findChild(item, "fleetHeading").text, "Join Laptop A's fleet")
    compare(findChild(item, "fleetMessage").text, "Receiving the invitation from Laptop A…")
    verify(!item.codeInput.visible)
    item.pairing = { phase: "incoming", peer_name: "Laptop A" }
    verify(item.codeInput.visible)
    item.pairing = { phase: "exchanging", peer_name: "Laptop A" }
    compare(findChild(item, "fleetMessage").text, "Finishing fleet setup…")
  }

  function test_invitationNamesSenderAndRequiresSixDigits() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "incoming", peer_name: "Laptop A", id: "session" }
    })
    submitSpy.target = item; submitSpy.clear()
    compare(findChild(item, "fleetHeading").text, "Join Laptop A's fleet")
    verify(findChild(item, "fleetMessage").text.indexOf("Laptop A") >= 0)
    var input = findChild(item, "fleetCodeInput")
    verify(input.visible)
    input.forceActiveFocus()
    keyClick(Qt.Key_0); keyClick(Qt.Key_1); keyClick(Qt.Key_2)
    keyClick(Qt.Key_3); keyClick(Qt.Key_4); keyClick(Qt.Key_5)
    compare(input.text, "012345")
    compare(submitSpy.count, 1)
    keyClick(Qt.Key_Return)
    compare(submitSpy.count, 1)
    compare(submitSpy.signalArguments[0][0], "012345")
    item.pairing = { phase: "exchanging", peer_name: "Laptop A" }
    compare(input.text, "")
    verify(!input.visible)
  }
  function test_wrongCodeReturnsToEmptyEntryAndAutomaticallyRetries() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "incoming", peer_name: "Laptop A", id: "session" }
    })
    submitSpy.target = item; submitSpy.clear()
    var input = item.codeInput
    input.forceActiveFocus()
    for (var i = 0; i < 6; ++i) keyClick(Qt.Key_1)
    compare(submitSpy.count, 1)
    item.pairing = { phase: "exchanging", peer_name: "Laptop A", id: "session" }
    item.pairing = { phase: "incoming", peer_name: "Laptop A", id: "session", error: "Incorrect code. Try again." }
    compare(input.text, "")
    verify(findChild(item, "fleetCodeError").visible)
    tryCompare(input, "activeFocus", true)
    input.insert(0, "01234")
    compare(submitSpy.count, 1)
    keyClick(Qt.Key_5)
    compare(submitSpy.count, 2)
    compare(submitSpy.signalArguments[1][0], "012345")
  }
  function test_digitBoxesSupportEditingAndPaste() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "incoming", peer_name: "Laptop A", id: "session" }
    })
    submitSpy.target = item; submitSpy.clear()
    var input = item.codeInput
    for (var i = 0; i < 6; ++i) {
      var box = findChild(item, "fleetCodeDigit" + i)
      verify(box !== null)
      compare(box.width, box.height)
    }
    input.forceActiveFocus()
    keyClick(Qt.Key_0); keyClick(Qt.Key_1); keyClick(Qt.Key_2)
    keyClick(Qt.Key_3); keyClick(Qt.Key_4)
    compare(findChild(item, "fleetCodeDigit0").digit, "0")
    compare(findChild(item, "fleetCodeDigit4").digit, "4")
    mouseClick(findChild(item, "fleetCodeDigit2"))
    keyClick(Qt.Key_9)
    compare(input.text, "01934")
    keyClick(Qt.Key_Backspace)
    compare(input.text, "0134")
    input.selectAll(); input.copy(); input.clear(); input.paste()
    compare(input.text, "0134")
    keyClick(Qt.Key_A)
    compare(input.text, "0134")
    compare(submitSpy.count, 0)
    input.text = "012345"; input.selectAll(); input.copy(); input.clear(); input.paste()
    compare(submitSpy.count, 1)
    compare(submitSpy.signalArguments[0][0], "012345")
  }
  function test_retryStillClearsEntryWhenStatusUpdatesAreCoalesced() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "incoming", peer_name: "Laptop A", id: "session", code_attempt: 0 }
    })
    submitSpy.target = item; submitSpy.clear()
    item.codeInput.forceActiveFocus()
    for (var i = 0; i < 6; ++i) keyClick(Qt.Key_1)
    compare(submitSpy.count, 1)
    item.pairing = { phase: "incoming", peer_name: "Laptop A", id: "session", code_attempt: 1,
      error: "Incorrect code. Try again." }
    compare(item.codeInput.text, "")
    verify(item.codeInput.enabled)
    tryCompare(item.codeInput, "activeFocus", true)
    for (var j = 0; j < 6; ++j) keyClick(Qt.Key_2)
    compare(submitSpy.count, 2)
  }
  function test_inviterDisplaysCodeWithoutCancelButton() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "inviting", peer_name: "Laptop B", initiator: true, code: "012345" }
    })
    cancelSpy.target = item; cancelSpy.clear()
    compare(findChild(item, "fleetDisplayCode").text, "012 345")
    verify(!findChild(item, "fleetCodeInput").visible)
    verify(!findChild(item, "fleetCancel").visible)
    mouseClick(findChild(item, "fleetClose"))
    compare(cancelSpy.count, 1)
  }
  function test_invitationCountdownAndClose() {
    var now = Date.now()
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "inviting", peer_name: "Laptop B", initiator: true, code: "012345", expires_ms: now + 102000 }
    })
    item.nowMs = now
    compare(findChild(item, "fleetHeading").text, "Add to fleet")
    var expiry = findChild(item, "fleetCodeExpiry")
    compare(expiry.text, "EXPIRES IN 01:42")
    item.nowMs = now + 1000
    compare(expiry.text, "EXPIRES IN 01:41")
    item.nowMs = now + 103000
    compare(expiry.text, "EXPIRES IN 00:00")
    compare(findChild(item, "fleetWaiting").text, "Waiting for Laptop B…")
    cancelSpy.target = item; cancelSpy.clear()
    mouseClick(findChild(item, "fleetClose"))
    compare(cancelSpy.count, 1)
  }
  function test_finishingSetupCanBeDismissedButNotCancelled() {
    var item = createTemporaryObject(pane, this)
    cancelSpy.target = item; cancelSpy.clear()
    dismissSpy.target = item; dismissSpy.clear()
    var phases = ["exchanging", "waiting_ack", "waiting_commit", "waiting_done"]
    for (var i = 0; i < phases.length; ++i) {
      item.pairing = { phase: phases[i], peer_name: "Laptop B" }
      verify(!findChild(item, "fleetCancel").visible)
      mouseClick(findChild(item, "fleetClose"))
      item.forceActiveFocus()
      keyClick(Qt.Key_Escape)
      compare(cancelSpy.count, 0)
      compare(dismissSpy.count, (i + 1) * 2)
    }
    item.pairing = { phase: "success", peer_name: "Laptop B" }
    verify(findChild(item, "fleetCancel").visible)
  }
  function test_successOnlyAfterDaemonCompletion() {
    var item = createTemporaryObject(pane, this, {
      pairing: { phase: "waiting_commit", peer_name: "Laptop A" }
    })
    verify(findChild(item, "fleetHeading").text !== "Fleet joined")
    item.pairing = { phase: "success", peer_name: "Laptop A" }
    compare(findChild(item, "fleetHeading").text, "Fleet joined")
    compare(findChild(item, "fleetCancel").text, "Done")
  }
}
