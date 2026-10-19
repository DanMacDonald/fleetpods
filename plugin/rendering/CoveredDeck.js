.import "Raster.js" as Raster

// Sub-grid slivers of the case's top deck can survive the lid's depth test.
// Blend them into the adjacent outer shell as the lid closes. Use the final
// theme-specific tone, so neither black pinholes nor bright specks appear.
function blend(values, samples, view) {
  var cover = view.state === "" ? 0 : Math.max(0, 1 - view.lid / 0.15)
  if (!(cover > 0)) return
  var up = Raster.rotate(0, 1, 0, view.pitch, view.yaw, 0)
  function upward(i) {
    return up[0] * samples.normals[i * 3] + up[1] * samples.normals[i * 3 + 1]
      + up[2] * samples.normals[i * 3 + 2]
  }
  for (var i = 0; i < values.length; i++) {
    if (samples.surfaces[i] !== 1 || upward(i) < 0.95) continue
    for (var dy = 1; dy <= Math.ceil(view.rows * 0.06); dy++) {
      var j = i + dy * view.columns
      if (j >= values.length || samples.surfaces[j] !== 1) break
      if (upward(j) >= 0.85) continue
      values[i] += cover * (values[j] - values[i])
      break
    }
  }
}
