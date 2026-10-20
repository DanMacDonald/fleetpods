.import "../../../plugin/DotMesh.js" as Live
.import "Light.js" as Light
.import "Parameters.js" as Parameters

function frame(geometry, state, yaw, pitch, lid, pose, settings, baseline) {
  var options = Parameters.normalize(settings)
  var dotPitch = baseline ? 3 : options.dotPitch
  var extent = state === "" ? 141 : 213
  var n = Math.floor(extent / dotPitch)
  var zoom = 0.98 * (state === "" ? 1.38 : 1)
  var values
  if (baseline) {
    values = Live.frame(geometry, state, lid, 0, yaw, pitch, n, n, true,
      undefined, zoom, undefined, pose, state === "")
  } else {
    var scene = Live.parts(geometry, state, lid, 0, undefined, undefined, pose)
    for (var i = 0; i < scene.length; i++) {
      if (scene[i].mesh.matte) scene[i].shade = options.recessShade
    }
    var view = {state: state, lid: lid, yaw: yaw, pitch: pitch, columns: n, rows: n,
      scale: n / (state === "" ? 2.95 : 3.6) * zoom, study: options}
    if (state === "") {
      values = new Float32Array(n * n)
      for (var y = -1; y <= 1; y += 2) for (var x = -1; x <= 1; x += 2) {
        view.sampleX = x * 0.25; view.sampleY = y * 0.25
        var sample = Light.render(scene, view).values
        for (var i = 0; i < values.length; i++) values[i] += sample[i] * 0.25
      }
    } else values = Light.render(scene, view).values
  }
  return {values: values, columns: n, pitch: dotPitch,
    dotSize: baseline ? 2 : Math.min(options.dotSize, dotPitch),
    paletteCurve: baseline ? Parameters.defaults().paletteCurve : options.paletteCurve,
    paletteShift: baseline ? Parameters.defaults().paletteShift : options.paletteShift,
    contrast: baseline ? Parameters.defaults().contrast : options.contrast,
    contrastPivot: baseline ? Parameters.defaults().contrastPivot : options.contrastPivot}
}
