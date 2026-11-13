pragma Singleton
import QtQuick

// The console test needs spacing and type sizes without starting the Quickshell runtime.
QtObject {
  function space(value) { return value }
  function duration(value) { return value }
  property QtObject font: QtObject {
    property int caption: 12
    property int title: 14
    property int body: 12
  }
}
