// deno run --allow-read tools/artwork/fresnel/test.js
import { loadQmlJs } from '../load_qml_js.js'
const Dark = loadQmlJs(new URL('Dark.js', import.meta.url))
const Mesh = loadQmlJs(new URL('Mesh.js', import.meta.url))
const Blend = loadQmlJs(new URL('Blend.js', import.meta.url))
const Original = loadQmlJs(new URL('../../../plugin/DotMesh.js', import.meta.url))
const source = Deno.readTextFileSync(new URL('../../../plugin/artwork/AirPodsMesh.js', import.meta.url))
const geometry = new Function(source.replace(/^\.pragma library\s*/, '') + '; return geometry')()
const settings = {fresnelOnly: true, strength: 1, power: 1.5, white: 0.65, fill: 0.2, face: 0.85}
function check(condition, message) { if (!condition) throw Error(message) }
check(Dark.fresnelTone([0, 0, 1], 1, settings) === 0, 'front-facing surface has no residual light')
check(Dark.fresnelTone([1, 0, 0], 1, settings) === 1.5, 'grazing surface reaches the highlight')
check(Dark.fresnelTone([1, 0, 0], 1, settings) === Dark.fresnelTone([0, -1, 0], 1, settings),
  'pure Fresnel must not favor the key-light direction')
check(Math.abs(Dark.fresnelTone([1, 0, 0], 0.1, settings) - 0.15) < 1e-12, 'retain material reflectance')
for (const state of ['', 'open', 'closed']) {
  const args = [geometry, state, state === 'closed' ? 0 : 1, 0, 0, 0.24, 47, 47,
    false, undefined, 0.98, undefined, 1, true, undefined]
  const actual = Mesh.frame(...args, settings)
  const changedFill = Mesh.frame(...args, {...settings, fill: 0.6, face: 0.5})
  check(actual.every((v, i) => v === changedFill[i]), 'fill/face controls cannot leak into Fresnel-only mode')
  check(actual.some(v => v > 0) && actual.every(v => Number.isFinite(v) && v >= 0 && v <= 1.5), 'bounded visible reflection')
  check(Mesh.frame(...args, {...settings, strength: 0}).every(v => v === 0), 'zero reflection means no illumination')
}
const background = {r: 0.1, g: 0.12, b: 0.15}
const palette = Dark.dotStyle(3, background, {r: 0.4, g: 0.4, b: 0.4}, settings)
check(palette.base === background && palette.level(0.001) === 0, 'no ambient floor in the palette')
const fresnel = [0.4, 0.5, 0.6], baseline = [0.8, 0.9, 1]
check(Blend.combine(fresnel, baseline, 0).every((v, i) => v === fresnel[i]), 'zero overlay shows Fresnel alone')
check(Blend.combine(fresnel, baseline, 1).every((v, i) => v === baseline[i]), 'full overlay restores brighter baseline shading')
check(Blend.combine(fresnel, [0, 0, 0], 1).every((v, i) => v === fresnel[i]), 'baseline overlay cannot erase bright Fresnel edges')
check(Blend.combine(fresnel, baseline, 0.5).every((v, i) => Math.abs(v - (fresnel[i] + baseline[i]) / 2) < 1e-12), 'overlay strength blends colors continuously')
for (const state of ['', 'open', 'closed']) {
  const args = [geometry, state, state === 'closed' ? 0 : 1, 0, 0, 0.24,
    47, 47, false, undefined, 0.98, undefined, 1, true, undefined]
  const original = Original.frame(...args)
  const options = {...settings, baseline: true, baselineFill: 0.57, baselineFace: 0.73}
  const neutral = Mesh.frame(...args, options)
  check(neutral.every((v, i) => v === original[i]), 'baseline controls can reproduce the plugin exactly')
  const filled = Mesh.frame(...args, {...options, baselineFill: 0.8})
  const dimmed = Mesh.frame(...args, {...options, baselineFace: 0.5})
  check(filled.some((v, i) => v > neutral[i] + 0.05), 'baseline fill changes frontal illumination')
  check(dimmed.some((v, i) => v < neutral[i] - 0.05), 'baseline brightness changes face shading')
  const pure = Mesh.frame(...args, settings)
  const unrelated = Mesh.frame(...args, {...settings, baselineFill: 0.8, baselineFace: 0.5})
  check(pure.every((v, i) => v === unrelated[i]), 'baseline controls cannot alter the Fresnel column')
}
console.log('Fresnel-only isolation, material response, palette, and case/pod states passed.')
