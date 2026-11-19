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
  readonly property bool volume: presentation.phase === "volume"
  // A volume card has its bar on the right, so the artwork's ring carries the charge.
  readonly property bool chargeRing: volume && presentation.battery >= 0
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
        objectName: "notificationRing"
        ringColor: root.chargeRing ? charge.fillColor : Commons.Color.notifications.text
        spinning: root.presentation.phase !== "playing" && !root.volume
        sweep: 1.6
        doneSweep: Math.PI * 2 * (root.chargeRing ? Math.min(100, root.presentation.battery) / 100 : 1)
        doneOpacity: spinning ? 0 : 1
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
      width: Math.max(0, root.width - x - (loudness.visible ? loudness.width + Commons.Style.space(12)
        : charge.visible ? charge.width + Commons.Style.space(12) : 0))
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
        visible: text !== ""
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
      objectName: "notificationBattery"
      anchors.right: parent.right
      y: device.y
      width: ringSize
      height: ringSize
      ringSize: root.iconSize
      captionInside: true
      level: root.presentation.battery
      visible: level >= 0 && !root.volume
      foreground: Commons.Color.notifications.text
      lowColor: Commons.Color.urgent
      fontFamily: "Liberation Sans"
    }
    Column {
      id: loudness
      objectName: "notificationVolume"
      readonly property real fraction: Math.max(0, Math.min(1, (root.presentation.volume || 0) / 100))
      visible: root.volume
      anchors.right: parent.right
      y: device.y + (device.height - height) / 2
      width: Math.min(Commons.Style.space(112), root.width * 0.36)
      spacing: Commons.Style.space(6)
      Item {
        width: parent.width
        height: caption.height
        Text {
          id: caption
          text: "Volume"
          color: Commons.Color.notifications.text
          opacity: 0.8
          font.family: "Liberation Sans"
          font.pixelSize: Commons.Style.font.body
        }
        Text {
          anchors.right: parent.right
          text: Math.round(loudness.fraction * 100) + "%"
          color: Commons.Color.notifications.text
          font.family: "Liberation Sans"
          font.pixelSize: Commons.Style.font.body
          font.bold: true
        }
      }
      Rectangle {
        width: parent.width
        height: Commons.Style.space(6)
        radius: height / 2
        color: Qt.rgba(Commons.Color.notifications.text.r, Commons.Color.notifications.text.g,
          Commons.Color.notifications.text.b, 0.2)
        Rectangle {
          objectName: "notificationVolumeFill"
          height: parent.height
          radius: parent.radius
          width: Math.max(loudness.fraction > 0 ? height : 0, parent.width * loudness.fraction)
          color: Commons.Color.notifications.text
          Behavior on width { NumberAnimation { duration: Motion.duration(Commons.Style, 90) } }
        }
      }
    }
  }
}
