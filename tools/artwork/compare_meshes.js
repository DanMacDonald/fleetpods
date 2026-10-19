// deno run --allow-read tools/artwork/compare_meshes.js /tmp/fleetpods-full.js
if (Deno.args.length !== 1) throw new Error('Pass the path of a --full Blender export')
const load = path => new Function(Deno.readTextFileSync(path).replace(/^\.pragma library\s*/, '') + '; return geometry')()
import { loadQmlJs } from './load_qml_js.js'
const Mesh = loadQmlJs(new URL('../../plugin/DotMesh.js', import.meta.url))
const small = load(new URL('../../plugin/artwork/AirPodsMesh.js', import.meta.url))
const full = load(Deno.args[0])
function check(condition, message) { if (!condition) throw new Error(message) }
let overlap = 0, union = 0, worst = 1, worstView = '', error = 0, cells = 0, views = 0
for (const state of ['', 'open', 'closed', 'open-empty']) {
  for (const size of [58, 96]) for (let step = 0; step < 24; step++) {
    const yaw = step * Math.PI / 12 + 0.13, lid = state === 'closed' ? 0 : 1
    const a = Mesh.frame(full, state, lid, 0, yaw, 0.24, size, size)
    const b = Mesh.frame(small, state, lid, 0, yaw, 0.24, size, size)
    let both = 0, either = 0
    for (let i = 0; i < a.length; i++) {
      if (a[i] || b[i]) either++
      if (a[i] && b[i]) { both++; error += Math.abs(a[i] - b[i]); cells++ }
    }
    overlap += both; union += either; views++
    if (both / either < worst) {
      worst = both / either
      worstView = `${state || 'pods'} ${size} cells, yaw=${yaw}`
    }
  }
}
check(overlap / union > 0.99, 'Aggregate silhouette overlap fell below 99%')
check(worst > 0.98, 'A sampled silhouette changed by more than 2%: ' + worstView)
check(error / cells < 0.04, 'Average shading error exceeds 4%')

// Isolate the black vent/hole material from white shells and metallic contacts.
// Using a nonzero shade preserves depth/occupancy while allowing a simple mask.
for (const geometry of [small, full]) for (const key of ['body', 'lid', 'bud']) {
  for (const batch of geometry[key]) batch.shade = batch.shade === 0.1 ? 0.001 : 1
}
let featureOverlap = 0, featureUnion = 0
for (let step = 0; step < 24; step++) {
  const yaw = step * Math.PI / 12 + 0.13
  const a = Mesh.frame(full, '', 1, 0, yaw, 0.24, 96, 96)
  const b = Mesh.frame(small, '', 1, 0, yaw, 0.24, 96, 96)
  for (let i = 0; i < a.length; i++) {
    const aa = a[i] > 0 && a[i] < 0.01, bb = b[i] > 0 && b[i] < 0.01
    if (aa || bb) featureUnion++
    if (aa && bb) featureOverlap++
  }
}
check(featureOverlap / featureUnion > 0.94, 'Vents/openings lost more than 6% coverage')
console.log(JSON.stringify({ views, silhouetteIoU: overlap / union, worstViewIoU: worst,
  worstView, meanShadingError: error / cells, darkFeatureIoU: featureOverlap / featureUnion }, null, 2))
