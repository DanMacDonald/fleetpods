import QtQuick
import qs.Commons as Commons

Item {
  id: root

  property color foregroundColor: Commons.Color.foreground
  property var displayed: null
  readonly property color foreground: displayed ? displayed.foreground : foregroundColor
  readonly property color backgroundColor: displayed ? displayed.background : Commons.Color.popups.background

  function refresh() {
    displayed = { foreground: foregroundColor, background: Commons.Color.popups.background }
  }

  // The shell updates foreground and popup background in the same event turn.
  onForegroundColorChanged: Qt.callLater(root.refresh)
  Component.onCompleted: refresh()

  Connections {
    target: Commons.Color.popups
    function onBackgroundChanged() { Qt.callLater(root.refresh) }
  }
}
