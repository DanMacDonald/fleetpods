import QtQuick
import qs.Commons
import "Motion.js" as Motion
import "ConnectionToast.js" as Toast

// One publisher across panels. The model owns presentation decisions; the
// native adapter owns publication. This host only samples status and time.
Item {
  id: root
  property var shell: null
  property var status: ({})
  property bool reachable: false
  property bool ready: false
  readonly property string publisher: Toast.token()
  property var presentation: Object.assign(Toast.presentation(Toast.initial(), 0, artworkDuration), {preview: false})
  readonly property int artworkDuration: Motion.duration(Style, 1000)

  function preview() { Toast.preview(Date.now()); return "Connection preview queued; audio is unchanged" }
  function probe() { return JSON.stringify(Toast.diagnostic) }
  function tick() {
    if (!ready || !Toast.acquire(publisher)) return
    var now = Date.now()
    presentation = Toast.advance(Toast.project(status, reachable, now), now,
      nativeCard.observation(now), artworkDuration, enabled)
    nativeCard.sync(presentation, now)
    record()
  }
  function record() {
    Toast.diagnostic = Object.assign(nativeCard.diagnostics(), {phase: presentation.phase,
      visible: presentation.visible, preview: presentation.preview, publisher: publisher})
  }

  NativeNotification {
    id: nativeCard
    shell: root.shell
    onChanged: Qt.callLater(root.tick)
  }
  // One sampler drives both the pose and dismissal. Native tree discovery is
  // throttled by its adapter; hidden painters skip rasterization.
  Timer {
    interval: root.presentation.animating ? 33 : 250
    running: true
    repeat: true
    onTriggered: root.tick()
  }
  onStatusChanged: tick()
  onReachableChanged: tick()
  onEnabledChanged: tick()
  Component.onCompleted: { ready = true; tick() }
  Component.onDestruction: {
    Toast.release(publisher)
  }
}
