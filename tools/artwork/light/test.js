// deno run --allow-read tools/artwork/light/test.js
import { loadQmlJs } from '../load_qml_js.js'
const Mesh = loadQmlJs(new URL('Mesh.js', import.meta.url))
const Light = loadQmlJs(new URL('Light.js', import.meta.url))
const Parameters = loadQmlJs(new URL('Parameters.js', import.meta.url))
const source = Deno.readTextFileSync(new URL('../../../plugin/artwork/AirPodsMesh.js', import.meta.url))
const geometry = new Function(source.replace(/^\.pragma library\s*/, '') + '; return geometry')()
const before = JSON.stringify(geometry)
function check(condition, message) { if (!condition) throw Error(message) }
const defaults = Parameters.defaults()
const Production = loadQmlJs(new URL('../../../plugin/rendering/Light.js', import.meta.url))
const chosen = JSON.parse(Deno.readTextFileSync(new URL('presets/2026-10-19.json', import.meta.url)))
check(Object.keys(chosen).every(key => Production.defaults()[key] === chosen[key]),
  'Production defaults must retain the approved study preset')
const style = Production.dotStyle(3, {r: 1, g: 1, b: 1}, {r: 0, g: 0, b: 0})
check(style.exponent === chosen.paletteCurve && Math.floor(style.size * 1.4 * 2) / 2 === chosen.dotSize,
  'Production ink curve and aligned dot size must match the study')
const stroke = new Float32Array(25)
stroke[12] = 0.9
const narrow = Light.resizeOutlines(stroke, 5, 5, 1)
const wide = Light.resizeOutlines(stroke, 5, 5, 3)
const between = Light.resizeOutlines(stroke, 5, 5, 2)
check(narrow[11] === 0 && wide[11] === stroke[12] && wide[12] === stroke[12],
  'Width expands the footprint while retaining central ink')
check(between[11] === wide[11] * 0.5 && between[12] === stroke[12],
  'Fractional width interpolates coverage rather than changing glyph size')
check(Light.resizeOutlines(stroke, 5, 5, 0).every(v => v === 0), 'Zero width removes contours')
// A surface with a distant internal crease separates the two edge classes.
// Exercise both fractional coverage and expanded strokes, not only defaults.
const n = 17, depth = new Float32Array(n * n).fill(-100)
const shading = new Float32Array(n * n).fill(0.1)
const features = new Float32Array(n * n)
for (let y = 2; y <= 14; y++) for (let x = 2; x <= 14; x++) depth[y * n + x] = 0
features[8 * n + 8] = 0.94
for (const outlineWidth of [0.5, 1, 3]) {
  const draw = options => Light.stipple(shading, depth, null, n, n, features, shading,
    {...defaults, exteriorWidth: outlineWidth, interiorWidth: outlineWidth, ...options})
  const both = draw({}), noExterior = draw({exteriorOutline: 0}), noInterior = draw({interiorOutline: 0})
  const outer = 8 * n + 2, inner = 8 * n + 8
  check(noExterior[outer] === shading[outer] && noExterior[inner] === both[inner],
    `Exterior contrast leaves internal outlines intact at width ${outlineWidth}`)
  check(noInterior[inner] === shading[inner] && noInterior[outer] === both[outer],
    `Internal contrast leaves exterior outlines intact at width ${outlineWidth}`)
  check(both[outer] > shading[outer] && both[inner] > shading[inner], 'Both classes carry visible ink')
  const neither = draw({exteriorOutline: 0, interiorOutline: 0})
  check(neither.every((v, i) => v === (depth[i] === -100 ? 0 : shading[i])),
    'Disabling contours preserves surface shading and background')
  for (const width of [0, 0.3, 1, 2, 3]) {
    const outerOnly = draw({exteriorWidth: width})
    const innerOnly = draw({interiorWidth: width})
    for (let y = 6; y <= 10; y++) for (let x = 6; x <= 10; x++) {
      const i = y * n + x
      check(outerOnly[i] === both[i], 'Exterior width cannot alter internal stroke or its coverage')
    }
    for (let x = 2; x <= 4; x++) {
      const i = 8 * n + x
      check(innerOnly[i] === both[i], 'Internal width cannot alter exterior stroke or its coverage')
    }
  }
}
const migrated = Parameters.normalize({outlineWidth: 0.4})
check(migrated.exteriorWidth === 0.4 && migrated.interiorWidth === 0.4,
  'Migrate shared width to both controls')
const explicit = Parameters.normalize({outlineWidth: 2, exteriorWidth: 0, interiorWidth: 0.5})
check(explicit.exteriorWidth === 0 && explicit.interiorWidth === 0.5 && !('outlineWidth' in explicit),
  'Explicit widths override old width and presets export only the new controls')
function render(state, options = defaults, baseline = false, yaw = 0) {
  return Mesh.frame(geometry, state, yaw, 0.24, state === 'closed' ? 0 : 1, 1, options, baseline)
}
for (const state of ['', 'open', 'closed', 'open-left', 'open-right', 'open-empty']) {
  for (const yaw of [0, 0.4]) {
    const baseline = render(state, defaults, true, yaw)
    const experiment = render(state, defaults, false, yaw)
    const error = Math.max(...baseline.values.map((v, i) => Math.abs(v - experiment.values[i])))
    check(error < 1e-6, `Default parity: ${state || 'pods'}, yaw ${yaw}: ${error}`)
  }
}
for (const control of Parameters.controls()) {
  let changes = false
  for (const state of ['', 'open-empty']) {
    // Contrast needs a nonzero band to exercise it; the chosen preset turns
    // internal contours off, so enable that band for this control check. Its
    // outline gain also saturates outline dots, so test overall strength at 1.
    const controlDefaults = control.key === 'interiorOutline' ? {...defaults, interiorWidth: 1}
      : control.key === 'outline' ? {...defaults, edgeGain: 1} : defaults
    const original = render(state, controlDefaults)
    for (const value of [control.from, control.to]) {
      const options = {...controlDefaults, [control.key]: value}
      const changed = render(state, options)
      check(changed.values.every(v => Number.isFinite(v) && v >= 0 && v <= 1), `Bounded ${control.key}`)
      changes ||= changed.values.length !== original.values.length
        || changed.values.some((v, i) => Math.abs(v - original.values[i]) > 1e-5)
        || changed.dotSize !== original.dotSize || changed.paletteCurve !== original.paletteCurve
        || changed.paletteShift !== original.paletteShift || changed.contrast !== original.contrast
        || changed.contrastPivot !== original.contrastPivot
      const baseline = render(state, options, true)
      const neutral = render(state, defaults, true)
      check(baseline.values.every((v, i) => v === neutral.values[i]), `Isolated ${control.key}`)
    }
  }
  check(changes, `Control has visible effect: ${control.key}`)
}
check(JSON.stringify(geometry) === before, 'Study must not mutate shared geometry')
const clamped = Parameters.normalize({outline: 99, podOutline: -1, dotPitch: 3.3, softness: NaN})
check(clamped.outline === 1.5 && clamped.podOutline === 0 && clamped.dotPitch === 3.5
  && clamped.softness === defaults.softness, 'Preset bounds and half-pixel spacing')
console.log('Light study: baseline parity, control effects, bounds, and isolation passed.')
