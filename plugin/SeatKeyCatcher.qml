import QtQuick

Item {
  property bool blocked: false
  signal moveRequested(int dx, int dy)
  signal activateRequested
  signal closeRequested
  signal tabRequested(int direction)

  focus: true
  Keys.priority: Keys.BeforeItem
  Keys.onPressed: function(event) {
    if (blocked) return
    // Printable keys must never turn ordinary typing into a device takeover.
    switch (event.key) {
    case Qt.Key_Left: moveRequested(-1, 0); break
    case Qt.Key_Right: moveRequested(1, 0); break
    case Qt.Key_Up: moveRequested(0, -1); break
    case Qt.Key_Down: moveRequested(0, 1); break
    case Qt.Key_Return:
    case Qt.Key_Enter: activateRequested(); break
    case Qt.Key_Escape: closeRequested(); break
    case Qt.Key_Tab:
    case Qt.Key_Backtab:
      tabRequested((event.modifiers & Qt.ShiftModifier) || event.key === Qt.Key_Backtab ? -1 : 1)
      break
    default: return
    }
    event.accepted = true
  }
}
