pragma Singleton
import QtQuick
// Control process completions without publishing real desktop notifications.
QtObject {
  property var calls: []
  function finish(index, id, code) {
    var process = calls[index].process
    process.stdout.text = id ? "u " + id : ""
    process.running = false
    process.exited(code || 0)
  }
}
