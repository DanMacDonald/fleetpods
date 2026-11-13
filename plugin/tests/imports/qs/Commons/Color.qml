pragma Singleton
import QtQuick

// The animation test needs colors without starting the Quickshell runtime.
QtObject {
  property color foreground: "#a9b1d6"
  property color urgent: "#ff0000"
  property QtObject notifications: QtObject {
    property color background: "#1a1b26"
    property color text: "#a9b1d6"
  }
  property QtObject popups: QtObject { property color background: "#1a1b26" }
}
