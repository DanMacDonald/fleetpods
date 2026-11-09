.import "Raster.js" as Raster
.import "CoveredDeck.js" as CoveredDeck

// Dark-background renderer: the original illuminated surfaces, including
// specular highlights and rim light for glossy materials. Authored matte
// materials use a softer diffuse range, without specular or rim lighting.
function render(scene, view) {
  var cover = view.state === "" ? 0 : Math.max(0, 1 - view.lid / 0.15)
  var samples = Raster.rasterize(scene, view, {
    vertexTone: artworkTone,
    surfaceData: cover > 0
  })
  CoveredDeck.blend(samples.light, samples, view)
  if (view.brightLidEdge) brightenLidEdge(samples, view)
  return { values: samples.light, tonal: false }
}

// At secondary-artwork density the thin white lip can fall between samples,
// leaving socket shading on the silhouette. Reserve one bright dot per column
// along the visible lid's top contour. The final depth-tested surface must be
// the lid: a pod in front, the case body and interior shading keep their tones.
function brightenLidEdge(samples, view) {
  if (!view.state || view.lid <= 0.85) return
  var amount = Math.min(1, (view.lid - 0.85) / 0.15)
  for (var x = 0; x < view.columns; x++) {
    for (var y = 0; y < view.rows; y++) {
      var i = y * view.columns + x
      if (!samples.surfaces[i]) continue
      if (samples.surfaces[i] === 2)
        samples.light[i] += (1 - samples.light[i]) * amount
      break
    }
  }
}

// Approved baseline-study settings, shared by the pods and every case state.
// Keep the original material lighting available to the isolated art study.
function artworkTone(n, shade, matte) {
  var tone = vertexTone(n, shade, matte)
  if (matte || shade < 0.85) return tone
  return Math.max(0, Math.min(1, 0.73 * (tone + 0.57 * Math.max(0, n[2]) * (shade - tone))))
}

function vertexTone(n, shade, matte) {
  var diffuse = Math.max(0, -0.45 * n[0] + 0.65 * n[1] + 0.61 * n[2])
  if (matte) return shade * (0.24 + 0.40 * Math.min(1, diffuse))
  var rim = Math.pow(1 - Math.abs(n[2]), 3) * 0.10
  var highlight = Math.pow(Math.max(0, -0.25 * n[0] + 0.36 * n[1] + 0.90 * n[2]), 24)
  return Math.min(1, (0.08 + 0.77 * diffuse + 0.28 * highlight + rim) * shade)
}

function dotStyle(pitch, background, shadow) {
  // Lift the lit surfaces to the theme foreground sooner, without lifting
  // the matte recess range (at most 0.384). Dot size stays uniform.
  return { levels: 16, size: Math.max(0.85, pitch * 0.40), base: shadow,
    exponent: 0.85, level: function(value) {
      if (value <= 0) return 0
      if (value > 0.40) value = 0.40 + (value - 0.40) * 1.85
      var t = Math.max(0, Math.min(1, (value - 0.16) / 0.64))
      return Math.max(1, Math.ceil(t * t * (3 - 2 * t) * 16))
    } }
}
