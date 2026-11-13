import QtQuick
QtObject {
  property var command: []
  property bool running: false
  property var stdout
  signal exited(int code)
  onRunningChanged: if (running) Transport.calls = Transport.calls.concat([{process: this, command: command}])
}
