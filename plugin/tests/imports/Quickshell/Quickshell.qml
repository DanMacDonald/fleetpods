pragma Singleton
import QtQuick
QtObject {
  property var commands: []
  function execDetached(command) { commands = commands.concat([command]) }
}
