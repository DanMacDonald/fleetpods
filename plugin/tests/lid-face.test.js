// Run with: deno run --allow-read plugin/tests/lid-face.test.js
import { loadQmlJs } from '../../tools/artwork/load_qml_js.js'
const Mesh = loadQmlJs(new URL('../DotMesh.js', import.meta.url))
const Raster = loadQmlJs(new URL('../rendering/Raster.js', import.meta.url))
const Dark = loadQmlJs(new URL('../rendering/Dark.js', import.meta.url))
const Light = loadQmlJs(new URL('../rendering/Light.js', import.meta.url))
const CoveredDeck = loadQmlJs(new URL('../rendering/CoveredDeck.js', import.meta.url))
const source = Deno.readTextFileSync(new URL('../artwork/AirPodsMesh.js', import.meta.url))
const geometry = new Function(source.replace(/^\.pragma library\s*/, '') + '; return geometry')()
function check(condition, message) { if (!condition) throw new Error(message) }

// Thin samples of the inner deck can survive depth testing at the closed
// seam. Match the adjacent shell, avoiding both white specks and dark pinholes.
let shadowedDeck = 0
for (const size of [64, 71, 80, 96, 128]) for (const yaw of [-0.1, 0, 0.1]) {
  const view = { state: 'closed', lid: 0, columns: size, rows: size,
    scale: size / 3.6 * 0.98, yaw, pitch: 0.24 }
  const scene = Mesh.parts(geometry, 'closed', 0, 0)
  const raw = Raster.rasterize(scene, view, { vertexTone: Dark.vertexTone, surfaceData: true })
  const closed = Dark.render(scene, view).values
  const closedInk = Light.render(scene, view).values
  const up = Raster.rotate(0, 1, 0, view.pitch, yaw, 0)
  for (let i = 0; i < closed.length; i++) {
    if (raw.surfaces[i] !== 1 || raw.light[i] < 0.6) continue
    const upward = up.reduce((sum, v, axis) => sum + v * raw.normals[i * 3 + axis], 0)
    if (upward < 0.99) continue
    check(closed[i] < raw.light[i] - 0.05,
      `closed deck must lose its overhead highlight at ${size} cells, sample ${i}`)
    const below = closed[i + size]
    check(Math.abs(closed[i] - below) < 0.04,
      `covered deck must join the shell without a dark pinhole at ${size} cells, sample ${i}`)
    check(Math.abs(closedInk[i] - closedInk[i + size]) < 0.04,
      `covered deck must join the light shell without dark flecks at ${size} cells, sample ${i}`)
    shadowedDeck++
  }
}
check(shadowedDeck > 10, 'exercise visible deck slivers at multiple grid sizes and angles')

// Lid coverage fades the correction; actual openings and other parts keep
// their own tone. Exercise both light and dark starting values.
for (const ink of [false, true]) for (const lid of [0, 0.075, 0.15, 1]) {
  const deck = ink ? 0.9 : 0.8, shell = ink ? 0.25 : 0.4
  const values = new Float32Array([deck, deck, deck, shell, shell, shell])
  const samples = {surfaces: [1, 2, 3, 1, 2, 3],
    normals: [0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1]}
  CoveredDeck.blend(values, samples, {state: 'open', lid, columns: 3, rows: 2, pitch: 0, yaw: 0})
  const expected = deck + Math.max(0, 1 - lid / 0.15) * (shell - deck)
  check(Math.abs(values[0] - expected) < 1e-6, 'covered deck fades continuously during closing')
  check(Math.abs(values[1] - deck) < 1e-6 && Math.abs(values[2] - deck) < 1e-6,
    'lid and pod surfaces must not receive case-deck correction')
}

// At popup density, the socket wall used to run directly into the top outline.
// Keep the white plastic lip readable without bleaching the socket below it.
for (const size of [80, 96, 128]) {
  const frame = Mesh.frame(geometry, 'open', 1, 0, 0, 0.24, size, size, true)
  for (const fraction of [0.35, 0.40, 0.60, 0.65]) {
    const x = Math.floor(size * fraction)
    let y = 0
    while (y < size && frame[y * size + x] === 0) y++
    check(frame[y * size + x] > 0.89, 'keep the thinner lid silhouette visible')
    check(frame[(y + 1) * size + x] <= 0.04,
      `keep a pale row between outline and socket at ${size} cells, column ${x}`)
    const interior = [2, 3, 4, 5].map(dy => frame[(y + dy) * size + x])
    check(Math.max(...interior) > 0.65, 'retain deep socket shading below the lip')
  }
}

// This actual rim location previously exposed the dark backing between two
// illuminated dots. It must shade as the continuous flat face around it.
for (const yaw of [-0.01, 0, 0.01]) {
  const frame = Mesh.frame(geometry, 'open', 1, 0, yaw, 0.24, 96, 96, false)
  check(Math.abs(frame[21 * 96 + 21] - frame[20 * 96 + 21]) < 1e-6,
    'the flat lid rim must not reveal a dark speck as it rotates')
}

// Validate the visible reference face over the animation range. A plane can
// brighten as a whole with rotation, but its samples must agree spatially.
for (const yaw of [-0.01, 0, 0.01]) {
  const frame = Mesh.frame(geometry, 'open', 1, 0, yaw, 0.24, 80, 80, false, 0, 0.98)
  for (const y of [21, 22]) for (const [edge, inside] of [[17, 18], [62, 61]]) {
    check(Math.abs(frame[y * 80 + edge] - frame[y * 80 + inside]) < 1e-6,
      'smoothed side rim must share the face lighting instead of retaining a dim groove')
  }
}

let checked = 0
for (const size of [64, 96, 132]) for (let step = 0; step <= 20; step++) {
  const yaw = -0.55 + step * 0.055
  const view = { columns: size, rows: size, scale: size / 3.6, yaw, pitch: 0.24 }
  const scene = Mesh.parts(geometry, 'open', 1, 0)
  const flat = scene.filter(p => p.surfaceId === 2 && p.shade === 1).map(p => ({
    ...p, mesh: { ...p.mesh, triangles: p.mesh.triangles.filter(f => f.every(i => {
      const v = p.mesh.vertices[i]
      return v[3] === 0 && v[4] === -1 && v[5] === 0
    })) },
  }))
  const plane = Raster.rasterize(flat, view, { vertexTone: Dark.vertexTone })
  const actual = Raster.rasterize(scene, view, { vertexTone: Dark.vertexTone })
  const localNormal = Raster.rotate(0, -1, 0, geometry.lidAngle, 0, 0)
  const expected = Dark.vertexTone(Raster.rotate(...localNormal, view.pitch, yaw, 0), 1)
  for (let i = 0; i < plane.depth.length; i++) {
    if (plane.depth[i] === -100 || actual.depth[i] > plane.depth[i] + 1e-6) continue
    const x = i % size, y = Math.floor(i / size)
    if (x < 1 || y < 1 || x + 1 >= size || y + 1 >= size
        || [i - 1, i + 1, i - size, i + size].some(j => plane.depth[j] === -100)) continue
    check(actual.surfaces[i] === 2 && actual.materials[i] === 1, 'the opaque rim must hide the backing')
    check(Math.abs(actual.light[i] - expected) < 1e-6, 'a flat lid face must use consistent lighting')
    checked++
  }
}
check(checked > 1000, 'the flat face must retain meaningful coverage at all preview scales')
console.log(`Flat lid lighting and backing occlusion passed across 63 animated views (${checked} samples)`)
