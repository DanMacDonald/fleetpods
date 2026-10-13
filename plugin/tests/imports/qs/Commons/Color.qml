pragma Singleton
import QtQuick

// The animation test needs colors without starting the Quickshell runtime.
QtObject {
  property color foreground: "#a9b1d6"
  property QtObject popups: QtObject { property color background: "#1a1b26" }
}
