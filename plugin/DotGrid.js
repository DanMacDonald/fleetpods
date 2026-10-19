.pragma library

// Shared physical dot grid for every 3D artwork state. Geometry framing must
// not change the pitch or glyph weight when switching between pods and case.
var pitch = 3
var scale = 1.4

function podsFrameSize(size, dotPitch) {
  return Math.floor(size / 0.90 / dotPitch) * dotPitch
}

function podsArtworkTop(size, artworkScale, dotPitch) {
  return size / 2 - podsFrameSize(size, dotPitch) * 0.22 * artworkScale
}
