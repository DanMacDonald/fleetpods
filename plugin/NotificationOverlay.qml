import QtQuick
import "Motion.js" as Motion
import qs.Commons as Commons

// FleetPods paints over the native content, leaving the native border, close
// button and input handling intact. The card owns all movement and clipping.
Item {
  id: root
  required property var source
  property var presentation: source.presentation
  property real iconSize: Commons.Style.space(40)
  property real contentOpacity: 1
  function motionState() {
    return { visible: visible, phase: presentation.phase, rotation: ring.rotation,
      running: spin.running, duration: spin.duration, width: width, height: height, battery: presentation.battery,
      artworkYaw: artwork.yaw, artworkProgress: presentation.artworkProgress }
  }
  function retire() {
    presentation = presentation
    source = null
    // Freeze one snapshot; the native parent owns the complete exit lifetime.
  }
  Rectangle {
    anchors.fill: parent
    // Keep the native text covered even while the deck fades its contents.
    color: Qt.rgba(Commons.Color.notifications.background.r,
      Commons.Color.notifications.background.g, Commons.Color.notifications.background.b, 1)
  }
  Item {
    anchors.fill: parent
    opacity: root.contentOpacity
    Item {
      id: device
      width: root.iconSize
      height: width
      y: (root.height - height) / 2
      NotificationArtwork {
        id: artwork
        objectName: "notificationDotArtwork"
        anchors.centerIn: parent
        anchors.verticalCenterOffset: root.presentation.variant === "max" ? -Commons.Style.space(2) : 0
        // Fit the complete silhouette inside the circle at every yaw, including
        // the lower cup corners; a square icon inset alone is insufficient.
        width: parent.width * (root.presentation.variant === "max" ? 0.60 : 0.759)
        height: width
        variant: root.presentation.variant
        foregroundColor: Commons.Color.notifications.text
        backgroundColor: Commons.Color.notifications.background
        progress: root.presentation.artworkProgress
      }
      ConnectionRing {
        id: ring
        anchors.fill: parent
        ringColor: Commons.Color.notifications.text
        spinning: root.presentation.phase !== "playing"
        sweep: 1.6
        doneSweep: Math.PI * 2
        doneOpacity: root.presentation.phase === "playing" ? 1 : 0
        RotationAnimation on rotation {
          id: spin
          from: 0; to: 360
          duration: Math.max(1, Motion.duration(Commons.Style, 1800))
          loops: Animation.Infinite
          running: ring.spinning && root.visible && Motion.duration(Commons.Style, 1800) > 0
        }
      }
    }
    Column {
      x: device.width + Commons.Style.space(12)
      y: device.y + (device.height - height) / 2
      width: Math.max(0, root.width - x - (charge.visible ? charge.width + Commons.Style.space(12) : 0))
      spacing: Commons.Style.space(3)
      Text {
        width: parent.width
        text: root.presentation.name
        textFormat: Text.PlainText
        elide: Text.ElideRight
        color: Commons.Color.notifications.text
        font.family: "Liberation Sans"
        font.pixelSize: Commons.Style.font.title
        font.bold: true
      }
      Text {
        width: parent.width
        text: root.presentation.text.detail
        textFormat: Text.PlainText
        elide: Text.ElideRight
        color: Commons.Color.notifications.text
        opacity: 0.8
        font.family: "Liberation Sans"
        font.pixelSize: Commons.Style.font.body
      }
    }
    BatteryIndicator {
      id: charge
      anchors.right: parent.right
      y: device.y
      width: ringSize
      height: ringSize
      ringSize: root.iconSize
      captionInside: true
      level: root.presentation.battery
      visible: level >= 0
      foreground: Commons.Color.notifications.text
      lowColor: Commons.Color.urgent
      fontFamily: "Liberation Sans"
    }
  }
}
