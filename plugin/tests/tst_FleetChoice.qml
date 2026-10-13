import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "FleetChoice"
  when: windowShown
  visible: true
  width: 460
  height: 380
  Component { id: pane; Pods.FleetChoice { width: 420; peerName: "lg-gram" } }
  SignalSpy { id: leaveSpy; signalName: "leaveAndJoin" }
  SignalSpy { id: inviteSpy; signalName: "inviteInstead" }
  SignalSpy { id: dismissSpy; signalName: "dismiss" }
  function setupSpies(item) {
    leaveSpy.target = item; leaveSpy.clear()
    inviteSpy.target = item; inviteSpy.clear()
    dismissSpy.target = item; dismissSpy.clear()
  }
  function test_noOffersAnInvitationWithoutLeaving() {
    var item = createTemporaryObject(pane, this)
    setupSpies(item)
    compare(findChild(item, "fleetChoiceMessage").text, "You are currently in a fleet. Leave it?")
    mouseClick(findChild(item, "fleetChoiceNo"))
    compare(findChild(item, "fleetChoiceMessage").text, "Invite lg-gram to your fleet instead?")
    compare(leaveSpy.count, 0)
    compare(inviteSpy.count, 0)
    mouseClick(findChild(item, "fleetChoiceYes"))
    compare(inviteSpy.count, 1)
    compare(leaveSpy.count, 0)
  }
  function test_noTwiceLeavesEverythingAlone() {
    var item = createTemporaryObject(pane, this)
    setupSpies(item)
    mouseClick(findChild(item, "fleetChoiceNo"))
    mouseClick(findChild(item, "fleetChoiceNo"))
    compare(dismissSpy.count, 1)
    compare(leaveSpy.count, 0)
    compare(inviteSpy.count, 0)
  }
  function test_decliningIncomingInvitationEndsTheFlow() {
    var item = createTemporaryObject(pane, this, { incoming: true })
    setupSpies(item)
    mouseClick(findChild(item, "fleetChoiceNo"))
    compare(dismissSpy.count, 1)
    compare(item.stage, "leave")
    compare(leaveSpy.count, 0)
    compare(inviteSpy.count, 0)
  }
  function test_acceptingIncomingInvitationStillLeavesAndJoins() {
    var item = createTemporaryObject(pane, this, { incoming: true })
    setupSpies(item)
    mouseClick(findChild(item, "fleetChoiceYes"))
    compare(leaveSpy.count, 1)
    compare(inviteSpy.count, 0)
    compare(dismissSpy.count, 0)
  }
  function test_leavingNeedsYesAndCannotRepeatWhileBusy() {
    var item = createTemporaryObject(pane, this)
    setupSpies(item)
    compare(leaveSpy.count, 0)
    mouseClick(findChild(item, "fleetChoiceYes"))
    compare(leaveSpy.count, 1)
    compare(inviteSpy.count, 0)
    item.busy = true
    verify(!findChild(item, "fleetChoiceYes").enabled)
    verify(!findChild(item, "fleetChoiceNo").enabled)
    mouseClick(findChild(item, "fleetChoiceYes"))
    compare(leaveSpy.count, 1)
  }
  function test_peerNameIsPlainText() {
    var item = createTemporaryObject(pane, this, { peerName: "<img src='bad'>", stage: "invite" })
    compare(findChild(item, "fleetChoiceMessage").textFormat, Text.PlainText)
    verify(findChild(item, "fleetChoiceMessage").text.indexOf("<img") >= 0)
  }
}
