.import "../../../plugin/rendering/Raster.js" as Raster
.import "../../../plugin/rendering/Dark.js" as Original

// Study-only baseline lighting controls. Preserve the plugin's material
// shading and seam treatment while adjusting fill and white-face brightness.
function render(scene, view) {
  var cover = view.state === "" ? 0 : Math.max(0, 1 - view.lid / 0.15)
  var up = Raster.rotate(0, 1, 0, view.pitch, view.yaw, 0)
  var samples = Raster.rasterize(scene, view, {
    vertexTone: function(n, shade, matte) {
      var tone = Original.vertexTone(n, shade, matte)
      if (matte || shade < 0.85) return tone
      var fill = view.study.baselineFill
      var face = view.study.baselineFace
      return Math.max(0, Math.min(1, face * (tone + fill * Math.max(0, n[2]) * (shade - tone))))
    },
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


function dotStyle(pitch, background, shadow) {
  return Original.dotStyle(pitch, background, shadow)
}
