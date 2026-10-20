// Bounds are shared by the controls, preset loader, and renderer.
function controls() {
  return [
    {key: "exteriorWidth", label: "Exterior thickness · dots", from: 0, to: 3, step: 0.1, value: 0.1},
    {key: "exteriorOutline", label: "Exterior outline contrast", from: 0, to: 2, value: 2},
    {key: "interiorWidth", label: "Internal thickness · dots", from: 0, to: 3, step: 0.1, value: 0},
    {key: "interiorOutline", label: "Internal outline contrast", from: 0, to: 2, value: 2},
    {key: "outline", label: "Overall outline strength", from: 0, to: 1.5, value: 1.5},
    {key: "edgeGain", label: "Outline darkness · gain", from: 1, to: 4, value: 4},
    {key: "podOutline", label: "Pod outline contrast", from: 0, to: 1, value: 1},
    {key: "podShading", label: "Pod surface shading", from: 0, to: 1.5, value: 0.63},
    {key: "surfaceShading", label: "Surface shadow strength", from: 0, to: 2, value: 1.57},
    {key: "frontalLight", label: "Frontal light", from: 0, to: 1.5, value: 0.72},
    {key: "lightX", label: "Light direction · horizontal", from: -1, to: 1, value: -0.13},
    {key: "lightY", label: "Light direction · vertical", from: -1, to: 1, value: 0.14},
    {key: "softness", label: "Shadow falloff", from: 0.4, to: 3, value: 1.15},
    {key: "contactShadows", label: "Contact shadows", from: 0, to: 2, value: 0.8},
    {key: "recessRelief", label: "Shallow recess shading", from: 0, to: 2, value: 0.42},
    {key: "recessShade", label: "Recess material brightness", from: 0.25, to: 0.8, value: 0.57},
    {key: "ventInk", label: "Vent darkness", from: 0, to: 1, value: 0.8},
    {key: "paletteCurve", label: "Ink curve", from: 0.3, to: 2, value: 1.75},
    {key: "paletteShift", label: "Palette shift toward ink", from: 0, to: 0.6, value: 0},
    {key: "contrast", label: "Contrast", from: 1, to: 3, value: 3},
    {key: "contrastPivot", label: "Contrast pivot", from: 0.1, to: 0.9, value: 0.17},
    {key: "dotPitch", label: "Dot spacing · px", from: 2, to: 4, step: 0.5, value: 3},
    {key: "dotSize", label: "Dot size · px", from: 0.5, to: 3, step: 0.5, value: 2}
  ]
}

function defaults() {
  var result = {}, list = controls()
  for (var i = 0; i < list.length; i++) result[list[i].key] = list[i].value
  return result
}

function normalize(input) {
  var result = defaults(), list = controls()
  for (var i = 0; i < list.length; i++) {
    var c = list[i], value = input && input[c.key]
    // Older saved studies used one width. Keep their appearance when loading,
    // but let explicit per-edge widths (including zero) take precedence.
    if (value === undefined && input && (c.key === "exteriorWidth" || c.key === "interiorWidth"))
      value = input.outlineWidth
    if (typeof value !== "number" || !isFinite(value)) continue
    var step = c.step || 0.01
    result[c.key] = Math.max(c.from, Math.min(c.to, Math.round(value / step) * step))
  }
  return result
}
