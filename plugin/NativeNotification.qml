import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import "ConnectionToast.js" as Toast

// Owns the native publication and all knowledge of Omarchy's two card layouts.
// sync() supplies an immutable presentation; observation() reports only facts
// about that publication. Native callbacks never mutate the toast lifecycle.
Item {
  id: root
  property var shell: null
  property var presentation: ({visible: false, episode: 0})
  property int notificationId: 0
  property real sentAt: 0
  property string sentText: ""
  property bool failed: false
  property int publications: 0
  property var overlays: []
  property real scannedAt: 0
  signal changed()
  readonly property var nativeService: {
    if (!shell) return null
    // Read only to re-evaluate this binding when the host registers services.
    var registered = shell.services
    return shell.firstPartyServiceFor ? shell.firstPartyServiceFor("omarchy.notifications") : null
  }

  // Older Omarchy releases expose a popup model instead of keyed cards.
  function legacyIndex(id) {
    var model = nativeService ? nativeService.popupModel : null
    if (!model) return -1
    for (var i = 0; i < model.count; i++) {
      var row = model.get(i)
      if (row.app === "FleetPods" && Number(row.originalId) === id) return i
    }
    return -1
  }
  function nativeKey(id) {
    try {
      if (nativeService && nativeService.keyForOriginal) return nativeService.keyForOriginal(id)
      return legacyIndex(id) >= 0 ? "legacy:" + id : ""
    }
    catch (error) { return "" }
  }
  function dismiss(id) {
    if (!id) return
    var key = nativeKey(id)
    // The current native renderer retains visible snapshots after a sender
    // closes its D-Bus object. Use its existing dismissal method when present.
    if (key && nativeService.closeToast) nativeService.closeToast(key, "dismissed", false)
    else if (legacyIndex(id) >= 0 && nativeService.dismissPopup) nativeService.dismissPopup(legacyIndex(id))
    else Quickshell.execDetached(["busctl", "--user", "--timeout=3s", "call",
      "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications",
      "CloseNotification", "u", String(id)])
  }
  function clearOverlays(immediate) {
    for (var i = 0; i < overlays.length; ++i) {
      try {
        if (overlays[i]) {
          if (immediate) overlays[i].destroy()
          else overlays[i].retire()
        }
      } catch (error) { /* native parent already gone */ }
    }
    overlays = []
  }
  function close(immediate) {
    var id = notificationId
    notificationId = 0
    sentText = ""
    clearOverlays(immediate)
    dismiss(id)
  }

  // This is the only adapter for native layout internals. Attach to matching
  // cards on all displays, including hidden ones, so monitor changes are free.
  // A missing capability leaves the normal themed notification intact.
  function attach() {
    if (!nativeService || !notificationId) return
    var found = [], seen = [], budget = 3000
    function add(card, slot) {
      var existing = null
      for (var n = 0; n < card.children.length; n++)
        if (card.children[n].objectName === "fleetpods-connection-overlay") existing = card.children[n]
      if (!existing) existing = overlay.createObject(card, { nativeSlot: slot,
        objectName: "fleetpods-connection-overlay" })
      if (existing) found.push(existing)
    }
    function walk(item, depth, inCard) {
      if (!item || depth > 18 || --budget <= 0 || seen.indexOf(item) >= 0) return
      seen.push(item)
      try {
        if (item.objectName === "fleetpods-connection-overlay") return
        if (item.app === "FleetPods" && item.hasSmallIcon !== undefined && item.closeRequested
          && item.parent && item.parent.index === legacyIndex(notificationId)) {
          add(item, null)
          return
        }
        if (item.scene === nativeService && item.row && Number(item.row.originalId) === notificationId
          && item.row.app === "FleetPods") inCard = item
        if (inCard && item.slot !== undefined && item.pictured !== undefined && item.glyphed !== undefined) {
          add(inCard, item)
          return
        }
        var children = item.data || item.children || []
        for (var i = 0; i < children.length; i++) walk(children[i], depth + 1, inCard)
        var instances = item.instances || []
        for (var j = 0; j < instances.length; j++) walk(instances[j], depth + 1, inCard)
        if (item.contentItem) walk(item.contentItem, depth + 1, inCard)
      } catch (error) { /* unsupported native object: plain toast fallback */ }
    }
    walk(nativeService, 0, false)
    var attached = found.length > 0 && overlays.length === 0
    overlays = found
    if (attached) changed()
  }

  function observation(now) {
    var key = nativeKey(notificationId)
    var canTrack = nativeService && (nativeService.keyForOriginal || nativeService.popupModel)
    return { episode: presentation.episode, attached: overlays.length > 0, failed: failed,
      dismissed: notificationId > 0 && canTrack && now - sentAt > 500
        && (!key || !!(nativeService.leaving && nativeService.leaving[key])) }
  }

  function sync(next, now) {
    if (presentation.episode !== next.episode) {
      close()
      failed = false
    }
    presentation = next
    if (!next.visible) { close(); return }
    if (failed || sender.running) return
    // Sampling the artwork at 30 fps must not scan the native tree at 30 fps.
    if (now - scannedAt >= 250) { attach(); scannedAt = now }
    var message = next.text
    var signature = JSON.stringify(message)
    // An attached overlay updates in place; only the plain fallback replaces
    // native text. Replacing an attached card would animate a second toast.
    if (!notificationId || (!overlays.length && now - sentAt > 1000 && sentText !== signature)) {
      sender.episode = next.episode
      sender.signature = signature
      sender.command = ["busctl", "--user", "--timeout=3s", "--", "call", "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications", "org.freedesktop.Notifications", "Notify", "susssasa{sv}i",
        "FleetPods", String(notificationId), "", message.summary, message.body, "0", "3",
        "urgency", "y", "1", "transient", "b", "true", "omarchy-glyph", "s", "󰋋", String(Toast.LIMIT_MS)]
      sender.running = true
      publications++
    }
  }

  function diagnostics() {
    return { id: notificationId, overlays: overlays.length, native: !!nativeService,
      publications: publications, motion: overlays.map(function(item) { return item.motionState() }) }
  }

  Component {
    id: overlay
    // The native card owns geometry, stacking, redaction and exit lifetime.
    Item {
      id: mount
      required property var nativeSlot
      readonly property bool legacy: !nativeSlot
      x: legacy ? parent.contentLeftInset + Style.space(12) : nativeSlot.mapToItem(parent, 0, 0).x
      y: legacy ? parent.contentTopInset + Style.space(10) : nativeSlot.mapToItem(parent, 0, 0).y - nativeSlot.y
      width: Math.max(0, parent.width - x - (legacy ? parent.contentRightInset + Style.space(12) : Style.space(34)))
      height: legacy ? Math.max(0, parent.height - y - parent.contentBottomInset - Style.space(10))
        : Math.max(nativeSlot.height, parent.textOffset + parent.textBlock)
      visible: legacy || !parent.redacted
      z: 100
      function retire() { content.retire() }
      function motionState() { return content.motionState() }
      NotificationOverlay {
        id: content
        anchors.fill: parent
        source: root
        iconSize: mount.legacy ? Style.space(40) : mount.nativeSlot.width
        contentOpacity: mount.legacy ? 1 : mount.parent.contentOpacity
      }
    }
  }
  Process {
    id: sender
    property real episode: 0
    property string signature: ""
    stdout: StdioCollector { id: reply; waitForEnd: true }
    onExited: function(code) {
      var match = /^u (\d+)\s*$/.exec(reply.text.trim())
      var id = code === 0 && match ? Number(match[1]) : 0
      if (!root.presentation.visible || root.presentation.episode !== episode) {
        root.dismiss(id)
      } else if (!id) {
        root.failed = true
        root.close()
      } else {
        root.notificationId = id
        root.sentText = signature
        root.sentAt = Date.now()
        root.attach()
      }
      root.changed()
    }
  }
  Component.onDestruction: close(true)
}
