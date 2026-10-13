pragma Singleton
import QtQuick

// The console test needs spacing and type sizes without starting the Quickshell runtime.
QtObject {
  function space(value) { return value }
  property QtObject font: QtObject { property int caption: 12 }
}
