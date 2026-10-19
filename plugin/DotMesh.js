.import "rendering/Light.js" as Light
.import "rendering/Dark.js" as Dark

// Shared artwork scene and public entry point. Theme policy belongs to the
// selected renderer; both use the same projected, depth-tested geometry.

function parts(geometry, state, lid, time, rightLift, leftLift, budsArrival, podTurns) {
  if (rightLift === undefined) rightLift = state === "open-left" || state === "open-empty" ? 1 : 0
  if (leftLift === undefined) leftLift = state === "open-right" || state === "open-empty" ? 1 : 0
  var result = [], surfaceId = 0
  function add(batches, at, angle, mirror, scale, dockedPod) {
    surfaceId++
    for (var i = 0; i < batches.length; i++) {
      result.push({ mesh: batches[i], at: at, angle: angle,
        surfaceId: surfaceId, shade: batches[i].shade, mirror: mirror || 1, scale: scale || 1,
        dockedPod: !!dockedPod })
    }
  }
  function bud(side, docked, lift) {
    if (docked) {
      var turn = podTurns ? Math.max(0, Math.min(1, podTurns[side < 0 ? "left" : "right"] || 0)) : 0
      // Keep each physical pod's handedness throughout the return. Approach
      // with its tip toward the viewer, then turn into the well about Y while
      // restoring the seated pitch and stem lean. Never interpolate a mirror.
      var angle = [geometry.dockAngle + (-0.50 - geometry.dockAngle) * turn,
        side * (Math.PI - 1.05) * turn, side * 0.05 * turn]
      add(geometry.bud, [side * geometry.dock[0], geometry.dock[1] + (lift || 0) * 2.4, geometry.dock[2]],
        angle, side, 1, true)
    } else {
      // Front presentation of the pair: tips face inward on entry, then each
      // bud turns about world Y to show its tip beside the upright stem.
      // Use the descent's already-eased progress directly. A second easing
      // curve made the turn appear to stop before the vertical travel ended.
      var turn = budsArrival === undefined ? 1 : Math.max(0, Math.min(1, budsArrival))
      // Keep the inward entry yaw; finish with the tips angled toward camera.
      var turnAngle = (0.50 + Math.PI) * (1 - turn) + 1.05 * turn
      // Leave room for inward-facing tips on entry, then close the central
      // gap as they turn outward into the illustrated pair's compact pose.
      // Tuck the pair slightly inward while the rotating tips are widest.
      var separation = 0.55 - 0.05 * turn - 0.04 * Math.sin(Math.PI * turn)
      // Align the native stem axis vertically before the Y-axis turn. The
      // docked pitch leans the stems as the tips turn toward the viewer.
      add(geometry.bud, [side * separation, 0, 0],
        [-0.50, -side * turnAngle, -side * 0.05], -side, 1.2)
    }
  }
  if (state === "") {
    bud(-1, false)
    bud(1, false)
  } else {
    add(geometry.body, [0, 0, 0], [0, 0, 0])
    add(geometry.lid, geometry.hinge, [lid * geometry.lidAngle, 0, 0])
    if (lid > 0.03) {
      if (leftLift < 1) bud(-1, true, leftLift)
      // Keep the right pod's shading/occlusion identity when the left clears.
      surfaceId = 3
      if (rightLift < 1) bud(1, true, rightLift)
    }
  }
  return result
}

// Numeric grid for geometry checks and consumers that do not need the palette.
function frame(geometry, state, lid, time, yaw, pitch, columns, rows, ink, rightLift, zoom, leftLift, budsArrival, smoothSampling, podTurns) {
  return renderFrame(geometry, state, lid, time, yaw, pitch, columns, rows, ink, rightLift, zoom, leftLift, budsArrival, smoothSampling, podTurns).values
}

// Both renderers return { values: Float32Array, tonal: boolean }. The painter
// uses tonal to select background-to-ink versus shadow-to-light dot colors.
function renderFrame(geometry, state, lid, time, yaw, pitch, columns, rows, ink, rightLift, zoom, leftLift, budsArrival, smoothSampling, podTurns) {
  var renderer = ink ? Light : Dark
  var scene = parts(geometry, state, lid, time, rightLift, leftLift, budsArrival, podTurns)
  var view = {
    state: state, lid: lid, yaw: yaw, pitch: pitch, columns: columns, rows: rows,
    scale: Math.min(columns, rows) / (state === "" ? 2.95 : 3.6) * (zoom === undefined ? 1 : zoom)
  }
  if (!smoothSampling) return renderer.render(scene, view)
  // Integrate a small footprint per grid cell before palette quantization.
  // Every sample uses the same renderer and depth rules; only dot color varies.
  var values = new Float32Array(columns * rows)
  for (var y = -1; y <= 1; y += 2) {
    for (var x = -1; x <= 1; x += 2) {
      view.sampleX = x * 0.25
      view.sampleY = y * 0.25
      var sample = renderer.render(scene, view)
      for (var i = 0; i < values.length; i++) values[i] += sample.values[i] * 0.25
    }
  }
  return { values: values, tonal: !!ink }
}

// The canvas only paints squares. Palette, quantization, and dot weight are
// selected by the same renderer that produced the frame.
function dotStyle(tonal, pitch, background, foreground, shadow) {
  var renderer = tonal ? Light : Dark
  var style = renderer.dotStyle(pitch, background, shadow)
  style.colors = []
  for (var band = 1; band <= style.levels; band++) {
    var mix = Math.pow(band / style.levels, style.exponent), base = style.base
    style.colors[band] = [base.r + (foreground.r - base.r) * mix,
      base.g + (foreground.g - base.g) * mix, base.b + (foreground.b - base.b) * mix]
  }
  return style
}
