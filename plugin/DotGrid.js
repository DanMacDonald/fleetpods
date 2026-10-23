.pragma library

// Shared physical dot grid for every 3D artwork state. Geometry framing must
// not change the pitch or glyph weight when switching between pods and case.
var pitch = 3
var scale = 1.4

// Quantize in the window's physical pixels, not half logical pixels. A 3 px
// grid at 160% otherwise repeats a 4.8 px spacing and produces beat patterns.
function screenPitch(requested, pixelRatio) {
  return Math.max(2, Math.round(Math.max(1.5, requested) * pixelRatio)) / pixelRatio
}

function screenSize(requested, dotPitch, pixelRatio) {
  // At 100%, preserve the laptop's half-logical-pixel weight with coverage
  // antialiasing. Rounding 1.5 to 2 otherwise makes dark dots much heavier.
  // Higher-density displays keep their crisp whole-pixel glyphs.
  var nominal = Math.max(0.5, Math.floor(requested * 2) / 2)
  if (pixelRatio === 1) return Math.max(1, Math.min(dotPitch - 1, nominal))
  return Math.max(1, Math.min(Math.round(dotPitch * pixelRatio) - 1,
    Math.round(nominal * pixelRatio))) / pixelRatio
}

// Canvas rounds its backing image to physical pixels, but presents it at the
// unrounded logical extent. Match that image's physical size exactly so the
// scene graph cannot squeeze a gap or a dot at fractional display scales.
function canvasScale(extent, pixelRatio) {
  var physical = Math.max(1, extent * pixelRatio)
  return Math.round(physical) / physical
}
