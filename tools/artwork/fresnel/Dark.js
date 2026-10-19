.import "Raster.js" as Raster

// Dark-background renderer: the original illuminated surfaces, including
// specular highlights and rim light for glossy materials. Authored matte
// materials use a softer diffuse range, without specular or rim lighting.
function render(scene, view) {
  var only = view.study && view.study.fresnelOnly
  var cover = only || view.state === "" ? 0 : Math.max(0, 1 - view.lid / 0.15)
  var up = Raster.rotate(0, 1, 0, view.pitch, view.yaw, 0)
  var samples = Raster.rasterize(scene, view, {
    vertexTone: only ? function() { return 0 } : view.state === "" ? function(n, shade, matte) { return standaloneBase(n, shade, matte, view.study) } : surfaceTone,
    surfaceData: true
  })
  for (var i = 0; i < samples.light.length; i++) {
    if (samples.depth[i] === -100) continue
    if (!only && (samples.materials[i] < 0.85 || samples.matte[i])) continue
    var nx = samples.normals[i * 3], ny = samples.normals[i * 3 + 1], nz = samples.normals[i * 3 + 2]
    var length = Math.sqrt(nx * nx + ny * ny + nz * nz)
    if (length > 0) {
      var normal = [nx / length, ny / length, nz / length]
      samples.light[i] = only ? fresnelTone(normal, samples.materials[i], view.study)
        : edgeReflection(normal, samples.light[i], view.study)

    }
  }
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

// Isolate the view-angle term: no key light, ambient floor, fill, or specular.
// Material reflectance still distinguishes white plastic from dark openings.
function fresnelTone(n, shade, study) {
  var grazing = Math.pow(1 - Math.min(1, Math.abs(n[2])), study.power)
  return Math.min(1.5, 1.5 * study.strength * Math.min(1, shade) * grazing)
}

// A broad camera-side fill keeps the white stems readable under the heads.
// Preserve authored dark materials and matte recesses; the key light still
// supplies the highlights and the cylindrical shading on both pods.
function standaloneBase(n, shade, matte, study) {
  var tone = surfaceTone(n, shade, matte)
  if (matte || shade < 0.85) return tone
  // Leave headroom between the filled faces and the bright wrapping edges.
  // The rim supplies stem definition; a strong frontal fill flattens it out.
  var fill = study ? study.fill : 0.20
  return (study ? study.face : 0.85) * (tone + fill * Math.max(0, n[2]) * (shade - tone))
}

function vertexTone(n, shade, matte) {
  var tone = surfaceTone(n, shade, matte)
  return matte || shade < 0.85 ? tone : edgeReflection(n, tone)
}

function surfaceTone(n, shade, matte) {
  var diffuse = Math.max(0, -0.45 * n[0] + 0.65 * n[1] + 0.61 * n[2])
  if (matte) return shade * (0.24 + 0.40 * Math.min(1, diffuse))
  var rim = Math.pow(1 - Math.abs(n[2]), 3) * 0.10
  var highlight = Math.pow(Math.max(0, -0.25 * n[0] + 0.36 * n[1] + 0.90 * n[2]), 24)
  return Math.min(1, (0.08 + 0.77 * diffuse + 0.28 * highlight + rim) * shade)
}

function edgeReflection(n, tone, study) {
  // Grazing white-plastic surfaces catch a brighter environment reflection.
  // A broad shoulder survives the small dot grid and wraps around the tip and
  // stem cylinders. Keep a gentle upper-left bias without losing the far rim.
  // Values above 1 use the white-tinted palette.
  var grazing = Math.pow(1 - Math.min(1, Math.abs(n[2])), study ? study.power : 1.5)
  var direction = 0.75 + 0.25 * Math.max(0, -0.65 * n[0] + 0.75 * n[1])
  var fresnel = Math.min(1, grazing * 1.8) * direction
  return tone + (1.5 - tone) * fresnel * (study ? study.strength : 1)
}

function dotStyle(pitch, background, shadow, study) {
  var only = study && study.fresnelOnly
  // Lift the lit surfaces to the theme foreground sooner, without lifting
  // the matte recess range (at most 0.384). Dot size stays uniform.
  return { levels: 24, baseLevels: 16, highlightMix: study ? study.white : 0.65,
    size: Math.max(0.85, pitch * 0.40), base: only ? background : shadow,
    exponent: only ? 1 : 0.85, level: function(value) {
      if (value <= 0) return 0
      if (value > 1) return 16 + Math.min(8, Math.ceil((value - 1) * 16))
      if (only) return Math.round(value * 16)
      if (value > 0.40) value = 0.40 + (value - 0.40) * 1.60
      var t = Math.max(0, Math.min(1, (value - 0.16) / 0.64))
      return Math.max(1, Math.ceil(t * t * (3 - 2 * t) * 16))
    } }
}
