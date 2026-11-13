import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import ".." as Pods

TestCase {
  id: test
  name: "NativeNotification"
  when: windowShown
  width: 400; height: 100
  Component { id: adapter; Pods.NativeNotification {} }
  Component {
    id: host
    Item {
      id: service
      property bool legacy: false
      property var services: [service]
      function firstPartyServiceFor(name) { return service }
      property var keys: ({})
      property var leaving: ({})
      property var closed: []
      property var keyForOriginal: legacy ? null : function(id) { return keys[id] || "" }
      property var closeToast: legacy ? null : function(key) { closed = closed.concat([key]) }
      property var dismissPopup: function(index) { closed = closed.concat([index]) }
      property alias popupModel: model
      ListModel { id: model }
    }
  }
  Component {
    id: keyedCard
    Item {
      property var scene
      property var row: ({app: "FleetPods", originalId: 41})
      property bool redacted: false
      property real contentOpacity: 1
      property real textOffset: 0
      property real textBlock: 40
      width: 400; height: 80
      Item {
        x: 12; y: 20; width: 40; height: 40
        property var slot: null
        property bool pictured: false
        property bool glyphed: true
      }
    }
  }
  Component {
    id: legacyWrapper
    Item {
      property int index: 0
      property alias card: card
      Item {
        id: card
        property string app: "FleetPods"
        property bool hasSmallIcon: true
        signal closeRequested()
        property real contentLeftInset: 0
        property real contentRightInset: 0
        property real contentTopInset: 0
        property real contentBottomInset: 0
        width: 400; height: 80
      }
    }
  }
  function view(episode, phase, visible) {
    return {episode: episode, phase: phase || "connecting", visible: visible !== false,
      name: "AirPods", variant: "pro", battery: 92, artworkProgress: phase === "playing" ? 1 : 0,
      text: {summary: "AirPods " + phase, body: "Audio", detail: phase || "Connecting…"}}
  }
  function init() { Transport.calls = []; Quickshell.commands = [] }

  function test_cardUpdatesInPlace_data() { return [{tag:"current",legacy:false},{tag:"legacy",legacy:true}] }
  function test_cardUpdatesInPlace(data) {
    var service = createTemporaryObject(host, test, {legacy:data.legacy})
    service.keys = {41:"card-41"}
    if (data.legacy) service.popupModel.append({app:"FleetPods",originalId:41})
    var item = createTemporaryObject(data.legacy ? legacyWrapper : keyedCard, service,
      data.legacy ? {} : {scene:service})
    var card = data.legacy ? item.card : item
    var n = createTemporaryObject(adapter, test, {shell:service})
    n.sync(view(1000), 1000)
    compare(Transport.calls.length, 1)
    Transport.finish(0, 41)
    compare(n.observation(Date.now()).attached, true)
    compare(n.diagnostics().overlays, 1)
    n.sync(view(1000, "playing"), 2000)
    compare(Transport.calls.length, 1, "Playing changes the overlay, not the native notification")
    compare(n.diagnostics().motion[0].artworkProgress, 1)
    n.sync(view(1000, "playing", false), 3000)
    compare(service.closed.length, 1)
    var mount = findChild(card, "fleetpods-connection-overlay")
    verify(mount !== null, "native exit retains the cover")
    compare(mount.motionState().phase, "playing")
    n.sync(view(4000, "connecting"), 4000)
    compare(mount.motionState().phase, "playing", "a later toast cannot change the retired snapshot")
  }

  function test_lateReplyCannotAffectNewEpisode_data() {
    return [{tag:"success",id:41,code:0},{tag:"failure",id:0,code:1}]
  }
  function test_lateReplyCannotAffectNewEpisode(data) {
    var n = createTemporaryObject(adapter, test)
    n.sync(view(1000), 1000)
    n.sync(view(2000), 2000)
    compare(Transport.calls.length, 1, "one in-flight publisher")
    Transport.finish(0, data.id, data.code)
    compare(n.observation(2000).failed, false, "old failure cannot cancel the new attempt")
    n.sync(view(2000), 2000)
    compare(Transport.calls.length, 2)
    compare(n.diagnostics().id, 0)
    if (data.id) compare(Quickshell.commands[0].slice(-2), ["u","41"], "late success is closed")
    Transport.finish(1, 42)
    compare(n.diagnostics().id, 42)
  }

  function test_plainFallbackAndManualDismissal() {
    var service = createTemporaryObject(host, test)
    service.keys = {41:"card-41"}
    var n = createTemporaryObject(adapter, test, {shell:service})
    n.sync(view(1000), 1000)
    Transport.finish(0, 41)
    var now = Date.now()
    n.sync(view(1000, "playing"), now + 1001)
    compare(Transport.calls.length, 2, "plain fallback updates its native text")
    compare(Transport.calls[1].command[11], "41", "fallback replaces the existing ID")
    Transport.finish(1, 41)
    service.leaving = {"card-41":true}
    compare(n.observation(Date.now() + 600).dismissed, true)
  }

  function test_publicationFailureIsReportedAndNotRetried() {
    var n = createTemporaryObject(adapter, test)
    n.sync(view(1000), 1000)
    Transport.finish(0, 0, 1)
    compare(n.observation(2000).failed, true)
    n.sync(view(1000), 2000)
    compare(Transport.calls.length, 1)
    n.sync(view(3000), 3000)
    compare(Transport.calls.length, 2, "next episode can publish again")
  }
}
