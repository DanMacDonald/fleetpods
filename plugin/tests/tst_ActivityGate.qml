import QtQuick
import QtTest
import ".." as Pods

TestCase {
  name: "ActivityGate"
  Component { id: gate; Pods.ActivityGate {} }

  SignalSpy { id: resumes; signalName: "resumed" }

  function test_activeDesktopDoesNotSynthesizeMoreInput() {
    var item = createTemporaryObject(gate, this)
    resumes.target = item
    resumes.clear()
    compare(resumes.count, 0)
    item.idle = true
    var before = Date.now()
    item.idle = false
    compare(resumes.count, 1)
    var observed = resumes.signalArguments[0][0]
    verify(observed >= before && observed <= Date.now())
    wait(2100)
    compare(resumes.count, 1)
    compare(resumes.signalArguments[0][0], observed)
    item.idle = true
    item.idle = false
    compare(resumes.count, 2)
  }

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
