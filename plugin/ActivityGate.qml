import QtQuick

QtObject {
  property bool idle: false
  property bool observedIdle: false
  readonly property bool active: observedIdle && !idle
  // A newly created idle monitor starts awake without an input event.
  onIdleChanged: if (idle) observedIdle = true
}
