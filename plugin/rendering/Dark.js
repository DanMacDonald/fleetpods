.import "Raster.js" as Raster

// Dark-background renderer: the original illuminated surfaces, including
// specular highlights and rim light for glossy materials. Authored matte
// materials use a softer diffuse range, without specular or rim lighting.
function render(scene, view) {
  var cover = view.state === "" ? 0 : Math.max(0, 1 - view.lid / 0.15)
  var up = Raster.rotate(0, 1, 0, view.pitch, view.yaw, 0)
  var samples = Raster.rasterize(scene, view, {
    vertexTone: artworkTone,
    surfaceData: cover > 0
  })
  if (cover > 0) {
    // Sub-grid slivers of the covered deck should share the adjacent shell's
    // shading. Blackening them instead creates pinholes in the closed seam.
    function upward(i) {
      return up[0] * samples.normals[i * 3] + up[1] * samples.normals[i * 3 + 1]
        + up[2] * samples.normals[i * 3 + 2]
    }
    for (var i = 0; i < samples.light.length; i++) {
      if (samples.surfaces[i] !== 1 || upward(i) < 0.95) continue
      for (var dy = 1; dy <= Math.ceil(view.rows * 0.06); dy++) {
        var j = i + dy * view.columns
        if (j >= samples.light.length || samples.surfaces[j] !== 1) break
        if (upward(j) >= 0.85) continue
        samples.light[i] += cover * (samples.light[j] - samples.light[i])
        break
      }
    }
  }
  return { values: samples.light, tonal: false }
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
      if (value > 0.40) value = 0.40 + (value - 0.40) * 1.60
      var t = Math.max(0, Math.min(1, (value - 0.16) / 0.64))
      return Math.max(1, Math.ceil(t * t * (3 - 2 * t) * 16))
    } }
}
