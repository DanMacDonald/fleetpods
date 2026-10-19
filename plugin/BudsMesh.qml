import QtQuick
import "DotGrid.js" as Grid

// Standalone pair, framed like the existing two-pod illustration. The parent
// owns arrival progress so switching back to the case can interrupt the drop.
Item {
  id: root
  required property color foregroundColor
  required property color backgroundColor
  property real arrival: 1
  property real dotPitch: Grid.pitch
  property real artworkScale: 1
  readonly property real artworkTop: mesh.y + mesh.height * (0.5 - 0.22 * artworkScale)

  DotPods {
    id: mesh
    objectName: "budsMesh"
    // Center a whole number of cells, without a fractional strip on one side.
    width: Grid.podsFrameSize(root.width, effectiveDotPitch)
    height: width
    anchors.centerIn: parent
    anchors.alignWhenCentered: false
    caseState: ""
    budsArrival: root.arrival
    foregroundColor: root.foregroundColor
    backgroundColor: root.backgroundColor
    shadowColor: Qt.tint(root.backgroundColor, Qt.rgba(root.foregroundColor.r,
      root.foregroundColor.g, root.foregroundColor.b, 0.08))
    // Share the physical grid and glyph rules with every case state.
    dotPitch: root.dotPitch
    // Keep a full physical pixel of clearance when rounding glyphs away from
    // the original 200% grid, including larger whole-pixel dots at 100%.
    zoom: (0.98 - (pixelRatio === 2 ? 0 : 2 / pixelRatio / root.width)) * root.artworkScale
    smoothSampling: true
    yaw: 0
    pitch: 0.24
    animating: false
    animatePodLifts: false
  }
}
