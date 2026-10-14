import QtQuick
import Qt5Compat.GraphicalEffects

Item {
  id: root
  property bool phone: false
  /// Brightness tier: 0 dim/disabled, 1 active without ownership, 2 owner.
  property int level: 0
  /// Ownership brightness (tier 2). Playing is told apart by the effect
  /// alone: the glow and notes play while an owner plays, its brightness
  /// stays at tier 2 when paused.
  readonly property bool selected: level >= 2
  property bool playing: false
  readonly property int glowFadeMs: 400
  required property color lineColor
  required property color screenFill
  // Strokes blend toward the screen but stay opaque, so the ellipse never shows through.
  readonly property color strokeColor: selected ? lineColor : Qt.tint(
    Qt.rgba(screenFill.r, screenFill.g, screenFill.b, 1),
    Qt.rgba(lineColor.r, lineColor.g, lineColor.b, level === 1 ? 0.65 : 0.4))
  readonly property int artworkPadding: 15
  readonly property string assetPrefix: "assets/devices/" + (phone ? "phone" : "laptop")
  width: phone ? 20 : 35
  height: phone ? 32.5 : 22.5

  Item {
    id: musicNotes
    objectName: "musicNotes"
    anchors.fill: parent
    readonly property bool emitting: root.selected && root.playing && root.visible
    readonly property int lifetimeMs: 1800
    readonly property int streamCount: 3
    readonly property int notesPerStream: 3
    readonly property real streamSpacing: root.phone ? 6 : 10
    readonly property real riseHeight: 28
    readonly property real driftWidth: 1.5
    readonly property int swayMs: 2400
    property real sway: 0
    opacity: emitting ? 1 : 0
    visible: opacity > 0
    Behavior on opacity { NumberAnimation { duration: root.glowFadeMs; easing.type: Easing.OutSine } }
    NumberAnimation on sway {
      running: musicNotes.emitting
      from: 0; to: Math.PI * 2
      duration: musicNotes.swayMs
      loops: Animation.Infinite
      easing.type: Easing.Linear
    }

    Repeater {
      objectName: "musicStreams"
      model: musicNotes.streamCount * musicNotes.notesPerStream
      Text {
        id: note
        required property int index
        readonly property int stream: index % musicNotes.streamCount
        readonly property int beat: Math.floor(index / musicNotes.streamCount)
        property real progress: 0
        text: index % 2 === 0 ? "♪" : "♫"
        color: root.lineColor
        font.family: "sans-serif"
        font.pixelSize: 14
        x: (root.width - width) / 2 + (stream - 1) * musicNotes.streamSpacing
          + Math.sin(musicNotes.sway + stream * Math.PI * 2 / musicNotes.streamCount) * musicNotes.driftWidth
        y: root.height * 0.4 - (root.height * 0.4 + musicNotes.riseHeight) * progress - height / 2
        opacity: Math.min(1, progress * 6) * Math.pow(1 - progress, 1.2)
        SequentialAnimation {
          running: musicNotes.emitting
          PropertyAction { target: note; property: "progress"; value: 0 }
          PauseAnimation {
            duration: (note.beat + note.stream / musicNotes.streamCount) * musicNotes.lifetimeMs / musicNotes.notesPerStream
          }
          NumberAnimation {
            target: note; property: "progress"; from: 0; to: 1
            duration: musicNotes.lifetimeMs
            loops: Animation.Infinite
            easing.type: Easing.Linear
          }
        }
      }
    }
  }

  Item {
    x: -root.artworkPadding
    y: -root.artworkPadding
    width: root.width + root.artworkPadding * 2
    height: root.height + root.artworkPadding * 2

    Image {
      id: halo
      anchors.fill: parent
      source: root.assetPrefix + "-selected.png"
      visible: false
      mipmap: true
    }
    ColorOverlay {
      objectName: "halo"
      anchors.fill: parent
      source: halo
      color: root.lineColor
      opacity: root.selected && root.playing ? 1 : 0
      visible: opacity > 0
      Behavior on opacity { NumberAnimation { duration: root.glowFadeMs; easing.type: Easing.OutSine } }
    }
    Image {
      id: screen
      anchors.fill: parent
      source: root.assetPrefix + "-screen.png"
      visible: false
      mipmap: true
    }
    ColorOverlay {
      anchors.fill: parent
      source: screen
      color: Qt.darker(Qt.rgba(root.screenFill.r, root.screenFill.g, root.screenFill.b, 1), 1.2)
    }
    Image {
      id: outline
      anchors.fill: parent
      source: root.assetPrefix + "-unselected.png"
      visible: false
      mipmap: true
    }
    ColorOverlay {
      objectName: "outline"
      anchors.fill: parent
      source: outline
      color: root.strokeColor
    }
  }
}
