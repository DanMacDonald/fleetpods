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
    // Round the centered frame toward the left physical pixel. At 100%, a
    // half-pixel origin would clip the rightmost 2 px light-theme glyph.
    x: Math.floor((root.width - width) * pixelRatio / 2) / pixelRatio
    y: (root.height - height) / 2
    caseState: ""
    budsArrival: root.arrival
    foregroundColor: root.foregroundColor
    backgroundColor: root.backgroundColor
    shadowColor: Qt.tint(root.backgroundColor, Qt.rgba(root.foregroundColor.r,
      root.foregroundColor.g, root.foregroundColor.b, 0.08))
    // Share the physical grid and glyph rules with every case state.
    dotPitch: root.dotPitch
    // Whole-number display scales share the same logical dot weight and
    // geometry. Only fractional scales need extra rounding clearance; an
    // inset at 100% moves the stem across samples and creates a dim edge strip.
    zoom: (0.98 - (pixelRatio % 1 === 0 ? 0 : 2 / pixelRatio / root.width)) * root.artworkScale
    // Use one sample while turning; restore the four-sample finish at rest.
    smoothSampling: root.arrival >= 1
    yaw: 0
    pitch: 0.24
    animating: false
    animatePodLifts: false
  }
}
