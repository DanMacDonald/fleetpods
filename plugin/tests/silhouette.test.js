// Run with: deno run --allow-read plugin/tests/silhouette.test.js
import { loadQmlJs } from '../../tools/artwork/load_qml_js.js'
const Raster = loadQmlJs(new URL('../rendering/Raster.js', import.meta.url))
const Mesh = loadQmlJs(new URL('../DotMesh.js', import.meta.url))
function check(condition, message) { if (!condition) throw new Error(message) }
const size = 16, view = { columns: size, rows: size, scale: 1, pitch: 0, yaw: 0 }
function polygon(points, surfaceId = 1, shade = 1) {
  return { mesh: { vertices: points.map(([x, y]) => [x - size / 2, size / 2 - y, 0, 0, 0, 1]),
    triangles: [[0, 1, 2], [0, 2, 3]] }, surfaceId, shade,
    angle: [0, 0, 0], at: [0, 0, 0], mirror: 1, scale: 1 }
}
function rect(x0, y0, x1, y1, surfaceId = 1, shade = 1) {
  return polygon([[x0, y0], [x1, y0], [x1, y1], [x0, y1]], surfaceId, shade)
}
function notched(depth) {
  const top = [[2, 5.1], [7, 5.1], [7.2, depth], [8.8, depth], [9, 5.1], [14, 5.1]]
  return top.slice(1).map((b, i) => polygon([top[i], b, [b[0], 12], [top[i][0], 12]]))
}
for (const capture of [false, true]) {
  const render = scene => Raster.rasterize(scene, view, {
    vertexTone: () => 0.6, surfaceData: capture, groups: capture ? [0, 1] : [],
  })
  const shallow = render(notched(5.7))
  for (const x of [7, 8]) {
    const i = 5 * size + x
    check(shallow.depth[i] !== -100, 'a sub-cell reversal should read as a straight edge')
    check(Math.abs(shallow.light[i] - 0.6) < 1e-6, 'filled samples retain actual surface illumination')
    if (capture) {
      check(shallow.normals[i * 3 + 2] === 1 && shallow.materials[i] === 1, 'filled samples retain normal/material attributes')
      check(shallow.layers[1].depth[i] === shallow.depth[i], 'shading layers must agree with repaired coverage')
    }
  }
  const deep = render(notched(7.7))
  check(deep.depth[5 * size + 7] === -100 && deep.depth[6 * size + 7] === -100,
    'a deep geometric cutout must remain open')
  const distant = render(notched(6.1))
  check(distant.depth[5 * size + 7] === -100, 'do not fill a center more than a quarter cell from geometry')
  const hole = render([rect(2, 2, 14, 7.3), rect(2, 7.7, 14, 14), rect(2, 7.3, 7.3, 7.7), rect(7.7, 7.3, 14, 7.7)])
  check(hole.depth[7 * size + 7] === -100, 'even a sub-cell enclosed hole must stay empty')
  const divided = notched(5.7)
  divided[4].surfaceId = 2
  check(render(divided).depth[5 * size + 8] === -100, 'never bridge different objects')
  divided[4].surfaceId = 1
  divided[4].shade = 0.1
  check(render(divided).depth[5 * size + 8] === -100, 'never bridge different materials')
  const flat = render([rect(2, 5.7, 14, 12)])
  check(flat.depth.slice(5 * size, 6 * size).every(v => v === -100), 'a straight edge must not be dilated')
}

const asset = Deno.readTextFileSync(new URL('../artwork/AirPodsMesh.js', import.meta.url))
const geometry = new Function(asset.replace(/^\.pragma library\s*/, '') + '; return geometry')()
// At the installed popup framing the underside ridge formerly made a pair
// of two-row tabs. The rounded lid now has a broad, continuous widest section.
const lidOnly = { ...geometry, body: [], bud: [] }
for (const ink of [false, true]) for (const n of [71, 80]) {
  const frame = Mesh.frame(lidOnly, 'open', 1, 0, 0, 0.24, n, n, ink, 0, 0.98)
  const widths = []
  for (let y = 0; y < n; y++) {
    const xs = Array.from({ length: n }, (_, x) => x).filter(x => frame[y * n + x] > 0)
    if (xs.length) widths.push(xs.at(-1) - xs[0] + 1)
  }
  const widest = Math.max(...widths)
  check(widths.filter(w => w === widest).length >= 5, 'lid skirt must not form narrow side tabs')
  if (!ink) {
    for (let y = 0; y < n; y++) {
      const xs = Array.from({ length: n }, (_, x) => x).filter(x => frame[y * n + x] > 0)
      if (xs.length && xs.at(-1) - xs[0] + 1 === widest) {
        check(frame[y * n + xs[0]] > 0.4 && frame[y * n + xs.at(-1)] > 0.4,
          'outer lid rim must not expose black backing as gaps in its lit edge')
      }
    }
    if (n === 71) {
      // The 137 px case's grid exposed sideways dome normals on the white
      // lower lip. Its last rounded rows must read as a continuous lit rim.
      const occupied = []
      for (let y = 0; y < n; y++) {
        const xs = Array.from({ length: n }, (_, x) => x).filter(x => frame[y * n + x] > 0)
        if (xs.length) occupied.push({ y, xs })
      }
      for (const { y, xs } of occupied.slice(-3)) {
        for (const x of [...xs.slice(0, 2), ...xs.slice(-2)])
          check(frame[y * n + x] > 0.4, 'lower lid corners must not shade like missing dots at the enlarged popup size')
      }
    }
  }
}

// A complete pale row separates the upper recesses from the lid outline at
// popup scale, including the sloping shoulders and the one-pod-out state.
for (const state of ['open', 'open-left']) {
  const n = 80, frame = Mesh.frame(geometry, state, 1, 0, 0, 0.24, n, n, true, undefined, 0.98)
  for (let x = 23; x <= 56; x++) {
    let top = 0
    while (!frame[top * n + x] && top < n) top++
    check(frame[top * n + x] > 0.89, 'thinner lid outline stays visible')
    check(frame[(top + 1) * n + x] <= 0.025, 'recess must have a pale row below the outline')
  }
}
// The source lip's shallow bow used to produce two bumps at popup density,
// even though a denser preview appeared straight. Check both themes/sizes.
for (const ink of [false, true]) for (const size of [80, 96, 128, 192]) {
  const frame = Mesh.frame(geometry, 'open', 1, 0, 0, 0.24, size, size, ink)
  const top = []
  for (let x = Math.ceil(size * 0.34); x < size * 0.66; x++) {
    let y = 0
    while (y < size && frame[y * size + x] === 0) y++
    top.push(y)
  }
  check(new Set(top).size === 1, `front-facing lid lip must be level at ${size} cells in theme ${ink}`)
}
for (const ink of [false, true]) for (const yaw of [-0.425, -0.42, -0.4, -0.395, 0.415, 0.42, 0.425]) {
  const size = 132, frame = Mesh.frame(geometry, 'open', 1, 0, yaw, 0.24, size, size, ink)
  const top = Array.from({ length: size }, (_, x) => {
    for (let y = 0; y < size; y++) if (frame[y * size + x] > 0) return y
    return size
  })
  for (let x = 45; x < 90; x++) {
    if (top[x] <= top[x - 1]) continue
    for (let length = 1; length <= 2; length++) {
      check(top[x + length] >= top[x], `lid edge reverses at yaw ${yaw}, column ${x}, theme ${ink ? 'light' : 'dark'}`)
    }
  }
}
console.log('Sub-cell lid notches are smooth in both themes; holes, deep cutouts, separate surfaces, and straight edges are preserved')
