import QtQuick

// Mirrors the status log's rows (Model.statusLog) into the model the console draws. Rows only append, change in
// place, or leave from the front, so they are matched by id and a live attempt keeps its delegate. A ListModel role
// keeps one type, so a row's event reaches the console as its kind string.
Item {
  id: root

  property var rows: []
  readonly property alias entries: messages

  ListModel { id: messages }

  function eventKind(row) {
    return row.event && row.event.kind ? row.event.kind : ""
  }

  function sync() {
    var list = rows || []
    var first = list.length ? list[0].id : Infinity
    while (messages.count > 0 && messages.get(0).rowId < first) messages.remove(0)
    for (var i = 0; i < list.length; ++i) {
      var row = list[i]
      if (i < messages.count) {
        var shown = messages.get(i)
        if (shown.kind !== row.kind) messages.setProperty(i, "kind", row.kind)
        if (shown.retryAt !== (row.retryAt || 0)) messages.setProperty(i, "retryAt", row.retryAt || 0)
        if (shown.retryPeer !== (row.retryPeer || "")) messages.setProperty(i, "retryPeer", row.retryPeer || "")
        if (shown.eventKind !== eventKind(row)) messages.setProperty(i, "eventKind", eventKind(row))
        if (shown.message !== row.message) messages.setProperty(i, "message", row.message)
        if (shown.live !== row.live) messages.setProperty(i, "live", row.live)
        if (shown.device !== (row.event && row.event.device || "")) messages.setProperty(i, "device", row.event && row.event.device || "")
        if (shown.detail !== row.detail) messages.setProperty(i, "detail", row.detail)
      } else {
        messages.append({ rowId: row.id, time: Qt.formatTime(new Date(row.at), "HH:mm:ss"), message: row.message,
          kind: row.kind, eventKind: eventKind(row), live: row.live, detail: row.detail, retryAt: row.retryAt || 0,
          retryPeer: row.retryPeer || "", device: row.event && row.event.device || "" })
      }
    }
  }

  onRowsChanged: sync()
  Component.onCompleted: sync()
}
