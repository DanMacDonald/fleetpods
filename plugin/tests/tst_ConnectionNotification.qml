import QtQuick
import QtTest
import Quickshell.Io
import ".." as Pods
import "../ConnectionToast.js" as Toast

TestCase {
  name: "ConnectionNotification"
  when: windowShown
  Component { id: notification; Pods.ConnectionNotification {} }
  function status(state, intent) {
    return {modelName:"AirPods", isHeadset:false, ownershipIntent:intent,
      connectionLifecycle:{state:state}, audioTransportActive:state === "streaming"}
  }
  function init() { Transport.calls = []; Toast.reset() }
  function test_backgroundAndPublicationFailure() {
    var n = createTemporaryObject(notification, this, {reachable:true, status:status("control_ready", "release")})
    compare(Transport.calls.length, 0)
    n.status = status("linking", "claim")
    compare(Transport.calls.length, 1)
    compare(JSON.parse(n.probe()).visible, true)
    Transport.finish(0, 0, 1)
    tryVerify(function() { return !JSON.parse(n.probe()).visible })
    n.tick()
    compare(Transport.calls.length, 1, "failed publication does not loop")
  }
  function test_disabledFastTakeoverAndPublisherHandoff() {
    var n = createTemporaryObject(notification, this, {reachable:true, enabled:false,
      status:status("control_ready", "release")})
    n.status = status("streaming", "claim")
    compare(Transport.calls.length, 0)
    n.enabled = true
    compare(Transport.calls.length, 0, "enabling does not announce established playback")
    n.status = status("control_ready", "release")
    n.status = status("streaming", "claim")
    compare(Transport.calls.length, 1, "a later fast takeover publishes")
    Transport.finish(0, 41)
    n.enabled = false
    compare(JSON.parse(n.probe()).visible, false)
    n.destroy()
    wait(0)
    var next = createTemporaryObject(notification, this, {reachable:true, status:status("linking", "claim")})
    compare(Transport.calls.length, 2, "another panel can take over after publisher destruction")
  }

  function test_monitorChangeDoesNotReannounceTheSameAttempt() {
    var n = createTemporaryObject(notification, this, {reachable:true, status:status("linking", "claim")})
    compare(Transport.calls.length, 1)
    Transport.finish(0, 41)
    n.destroy()
    wait(0)
    var next = createTemporaryObject(notification, this, {reachable:true, status:status("linking", "claim")})
    compare(Transport.calls.length, 1, "publisher handoff retains the ongoing attempt baseline")
    compare(JSON.parse(next.probe()).visible, false)
    next.status = status("control_ready", "release")
    next.status = status("linking", "claim")
    compare(Transport.calls.length, 2, "a fresh takeover still notifies")
  }
}
