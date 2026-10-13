import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "ActivityGate"
  Component { id: gate; Pods.ActivityGate {} }

  function test_startupIsNotInput() {
    var item = createTemporaryObject(gate, this)
    compare(item.active, false)
  }

  function test_onlyRealResumeEnablesActivity() {
    var item = createTemporaryObject(gate, this)
    item.idle = true
    compare(item.active, false)
    item.idle = false
    compare(item.active, true)
    item.idle = true
    compare(item.active, false)
  }
}
