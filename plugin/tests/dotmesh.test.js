// Run with: deno run --allow-read plugin/tests/dotmesh.test.js
import { loadQmlJs } from "../../tools/artwork/load_qml_js.js"
const Mesh = loadQmlJs(new URL("../DotMesh.js", import.meta.url))
const Light = loadQmlJs(new URL("../rendering/Light.js", import.meta.url))
const Dark = loadQmlJs(new URL("../rendering/Dark.js", import.meta.url))
const Raster = loadQmlJs(new URL("../rendering/Raster.js", import.meta.url))
const asset = Deno.readTextFileSync(new URL("../artwork/AirPodsMesh.js", import.meta.url))
const geometry = new Function(asset.replace(/^\.pragma library\s*/, "") + "; return geometry")()
function check(condition, message) { if (!condition) throw new Error(message) }

// Public-interface snapshots, initially captured before separating renderers
// and updated for reviewed visual changes. Cover both themes, every pod/case
// state, lid motion, and multiple grids.
// After reviewing an intentional visual change, regenerate with
// deno run --allow-read --allow-write=plugin/tests/dotmesh-frames.json plugin/tests/dotmesh.test.js --update-frames
const fixturesUrl = new URL("dotmesh-frames.json", import.meta.url)
const fixtures = JSON.parse(Deno.readTextFileSync(fixturesUrl))
for (const fixture of fixtures) {
  const rendered = Mesh.renderFrame(geometry, ...fixture.args)
  check(rendered.tonal === fixture.args[7], "renderer selected the wrong palette")
  const hash = await crypto.subtle.digest("SHA-256", new Uint8Array(rendered.values.buffer))
  const digest = Array.from(new Uint8Array(hash), b => b.toString(16).padStart(2, "0")).join("")
  if (Deno.args.includes("--update-frames")) fixture.sha256 = digest
  else check(digest === fixture.sha256, "artwork frame changed: " + JSON.stringify(fixture.args))
}
console.log(`${fixtures.length} light/dark reference frames passed`)

// Standalone entry rotates rigid buds around Y, without changing their scale,
// handedness, or seating transforms used by any of the case states.
{
  const start = Mesh.parts(geometry, "", 1, 0, 0, 0, 0)
  const end = Mesh.parts(geometry, "", 1, 0, 0, 0, 1)
  const settled = Mesh.parts(geometry, "", 1, 0)
  check(start.length === end.length && start.length === geometry.bud.length * 2,
    "standalone entrance must retain both complete buds")
  for (let i = 0; i < start.length; i++) {
    check(start[i].mesh === end[i].mesh && start[i].mirror === end[i].mirror
      && start[i].scale === end[i].scale, "turning must not morph or exchange meshes")
    check(start[i].angle[0] === end[i].angle[0] && start[i].angle[2] === end[i].angle[2]
      && start[i].at[1] === end[i].at[1] && start[i].at[2] === end[i].at[2],
      "turn only about vertical Y while closing the horizontal gap")
    check(Math.abs(end[i].at[0]) < Math.abs(start[i].at[0])
      && Math.sign(end[i].at[0]) === Math.sign(start[i].at[0]),
      "buds should settle closer together without crossing sides")
    const rotation = Math.abs(start[i].angle[1] - end[i].angle[1])
    check(rotation > Math.PI * 0.75 && rotation < Math.PI,
      "entry must turn inward tips to the final outward presentation")
    check(end[i].angle.every((v, axis) => v === settled[i].angle[axis]),
      "startup defaults to the settled pose")
  }
  for (const state of ["closed", "open", "open-left", "open-right", "open-empty"]) {
    check(JSON.stringify(Mesh.parts(geometry, state, 1, 0, undefined, undefined, 0))
      === JSON.stringify(Mesh.parts(geometry, state, 1, 0, undefined, undefined, 1)),
      "standalone arrival must not affect case poses")
  }
}

// Theme selection controls the palette and grid weight as well as shading.
{
  const background = { r: 0.94, g: 0.93, b: 0.90 }
  const foreground = { r: 0.2, g: 0.23, b: 0.3 }, shadow = { r: 0.1, g: 0.12, b: 0.2 }
  const light = Mesh.dotStyle(true, 3, background, foreground, shadow)
  const dark = Mesh.dotStyle(false, 3, background, foreground, shadow)
  check(light.levels === 64 && dark.levels === 16, "each renderer must keep its palette resolution")
  check(light.size === 1.44 && dark.size === 3 * 0.40, "each renderer must keep its dot weight")
  check(light.level(0) === 0 && dark.level(0) === 0, "empty grid samples must not paint")
  check(light.level(0.001) === 0 && dark.level(0.001) === 1, "light fades to background; dark retains faint surfaces")
  for (const style of [light, dark]) {
    check(style.colors.slice(1).every(c => c.every(Number.isFinite)), "palette colors must remain finite")
    check(style.colors[style.levels].every((v, i) => Math.abs(v - [foreground.r, foreground.g, foreground.b][i]) < 1e-12),
      "full tone must reach the requested foreground color")
  }
  const contrast = Mesh.dotStyle(false, 3, {r: 0, g: 0, b: 0},
    {r: 1, g: 1, b: 1}, {r: 0.08, g: 0.08, b: 0.08})
  check(contrast.colors[contrast.level(0.2)][0] < 0.2,
    "dark recesses must retain near-background values")
  check(contrast.colors[contrast.level(0.65)][0] > 0.8,
    "lit faces must approach foreground without raising the shadows")
}
// Validate the imported asset at the boundary: malformed indices or normals
// can silently erase triangles or make the light-background mask unstable.
let triangles = 0
for (const key of ["body", "lid", "bud"]) {
  check(geometry[key].length > 0, "missing reference part: " + key)
  for (const batch of geometry[key]) {
    check(batch.shade > 0 && Number.isFinite(batch.shade), "material must retain depth coverage")
    for (const v of batch.vertices) {
      check(v.length === 6 && v.every(Number.isFinite), "invalid position/normal")
      check(Math.abs(Math.hypot(...v.slice(3)) - 1) < 0.0001, "normal must remain normalized")
    }
    if (batch.cavity) {
      check(batch.cavity.length === batch.vertices.length && batch.cavity.every(v => Number.isFinite(v) && v >= 0 && v <= 1),
        "cavity weights must align with mesh vertices and remain bounded")
    }
    for (const face of batch.triangles) {
      check(face.length === 3 && new Set(face).size === 3 && face.every(i =>
        Number.isInteger(i) && i >= 0 && i < batch.vertices.length), "invalid triangle")
    }
    triangles += batch.triangles.length
  }
}
for (const key of ["body", "lid"]) {
  const batch = geometry[key].find(b => b.contours)
  check(batch && batch.contours.length === 1, "both finger bevel boundaries must survive export")
  check(batch.contours[0].length >= 8 && batch.contours[0].every(i =>
    Number.isInteger(i) && i >= 0 && i < batch.vertices.length), "bevel landmarks must reference mesh vertices")
}

// A shallow boundary stays a connected single-cell line, but disappears behind
// foreground geometry (including another face of the same part) or when back-facing.
{
  const size = 16, depth = new Float32Array(size * size).fill(1)
  const surfaces = new Uint8Array(size * size).fill(1)
  const contour = { surfaceId: 1, points: [[2.5, 8.5, 1, 1, 1], [4.5, 4.5, 1, 1, 1],
    [11.5, 4.5, 1, 1, 1], [13.5, 8.5, 1, 1, 1]] }
  let traced = Light.meshContours([contour], depth, surfaces, size, size)
  for (let x = 5; x <= 10; x++) {
    check(traced.edges[4 * size + x] > 0.8, "the bevel boundary must have an uninterrupted outline")
    check(traced.edges[3 * size + x] === 0 && traced.edges[5 * size + x] === 0,
      "the bevel boundary must be one dot wide")
    check(traced.interior[6 * size + x] === 1 && traced.interior[10 * size + x] === 0,
      "only the bevel interior may soften the part seam")
  }
  for (const samePart of [false, true]) {
    depth.fill(2); surfaces.fill(samePart ? 1 : 2)
    traced = Light.meshContours([contour], depth, surfaces, size, size)
    check(traced.edges.every(v => v === 0) && traced.interior.every(v => v === 0),
      "hidden bevels must neither draw nor erase foreground edges")
  }
  depth.fill(1); surfaces.fill(1)
  for (const point of contour.points) point[4] = -1
  traced = Light.meshContours([contour], depth, surfaces, size, size)
  check(traced.edges.every(v => v === 0) && traced.interior.every(v => v === 0),
    "back-facing bevels must not appear on the back of the case")
}
console.log("Mesh bevel outlines, interior coverage, and occlusion passed")

check(triangles < 7000, "runtime asset exceeded its stored triangle budget")
check(geometry.bud.some(batch => batch.shade <= 0.1), "earbud vents/openings must survive export")

function checkSilhouette(light, ink, size, label, minimum = 0.9) {
  for (let y = 1; y < size - 1; y++) for (let x = 1; x < size - 1; x++) {
    const i = y * size + x
    if (!light[i]) continue
    const exposed = [-size, -1, 1, size]
      .some(offset => light[i + offset] === 0)
    if (exposed) check(ink[i] >= minimum, `the silhouette must never lose an edge dot: ${label} at ${x},${y}`)
  }
}
let frames = 0
for (const state of ["", "open", "closed", "open-left", "open-right", "open-empty"]) {
  for (const yaw of [-Math.PI, -Math.PI / 2, 0, Math.PI / 2, Math.PI]) {
    const light = Mesh.frame(geometry, state, state === "closed" ? 0 : 1, 0, yaw, 0.24, 64, 64)
    check(light.length === 4096, "one intensity per lattice cell")
    check(light.every(x => Number.isFinite(x) && x >= 0 && x <= 1), "finite, bounded illumination")
    const lit = light.filter(x => x > 0).length
    check(lit > 50 && lit < 2800, "the silhouette must be visible and leave transparent space: " + state)
    const ink = Mesh.frame(geometry, state, state === "closed" ? 0 : 1, 0, yaw, 0.24, 64, 64, true)
    check(ink.every((v, i) => Number.isFinite(v) && v >= 0 && v <= 1 && (v === 0 || light[i] > 0)),
      "ink must remain on the visible surface, without dots in the background")
    check(ink.some(v => v > 0), "ink must retain a visible shape: " + state)
    // Standalone shells use a softer contour; every boundary dot must still
    // survive rather than thinning the outline by removing grid samples.
    checkSilhouette(light, ink, 64, state + " yaw=" + yaw, state === "" ? 0.4 : 0.89)
    for (let x = 0; x < 64; x++) {
      check(light[x] === 0 && light[63 * 64 + x] === 0, "artwork must not clip vertically")
      check(light[x * 64] === 0 && light[x * 64 + 63] === 0, "artwork must not clip horizontally")
    }
    frames++
  }
}
// Include coarse grids and oblique views: diagonal outlines and thin stems
// must remain legible as the projected boundary moves from cell to cell.
// Cardinal boundaries trace an eight-connected, single-cell staircase; forcing
// all diagonal neighbors into the boundary would thicken it into a stroke.
let contourViews = 0
for (const size of [42, 58, 96]) for (const state of ["", "open", "closed"]) {
  for (let step = 0; step < 12; step++) {
    const yaw = step * Math.PI / 6 + 0.13
    const light = Mesh.frame(geometry, state, state === "closed" ? 0 : 1, 0, yaw, 0.24, size, size)
    const ink = Mesh.frame(geometry, state, state === "closed" ? 0 : 1, 0, yaw, 0.24, size, size, true)
    checkSilhouette(light, ink, size, `${state} grid=${size} yaw=${yaw}`, state === "" ? 0.4 : 0.89)
    contourViews++
  }
}
const front = Mesh.frame(geometry, "open", 1, 0, 0, 0.24, 64, 64)
const turn = Mesh.frame(geometry, "open", 1, 0, 0.8, 0.24, 64, 64)
check(front.some((v, i) => Math.abs(v - turn[i]) > 0.1), "rotation must change the projected surface")

// A bright face leaves bare paper inside its contour. A dark recess on that
// same face stays fully inked, regardless of the screen-space stipple pattern.
const light = new Float32Array(81), depth = new Float32Array(81), facing = new Float32Array(81)
depth.fill(-100)
for (let y = 1; y < 8; y++) for (let x = 1; x < 8; x++) {
  const i = y * 9 + x
  light[i] = 0.95; depth[i] = 1; facing[i] = 1
}
const bright = Light.stipple(light, depth, facing, 9, 9)
check(bright.some(v => v > 0), "the silhouette remains visible on a bright surface")
for (let y = 2; y < 7; y++) for (let x = 2; x < 7; x++) {
  check(bright[y * 9 + x] === 0, "bright interior must have no dots")
}
for (let y = 3; y < 6; y++) for (let x = 3; x < 6; x++) light[y * 9 + x] = 0.03
const recess = Light.stipple(light, depth, facing, 9, 9)
for (let y = 3; y < 6; y++) for (let x = 3; x < 6; x++) {
  check(recess[y * 9 + x] > 0.9, "ear openings and vents retain strong ink coverage")
}
check(recess.every((v, i) => depth[i] !== -100 || v === 0), "empty background stays empty")
const repeated = Light.stipple(light, depth, facing, 9, 9)
check(recess.every((v, i) => v === repeated[i]),
  "the same pose never reseeds the stipple pattern")
console.log(`dotmesh.test.js: ${frames} lighting views and ${contourViews} additional contour views passed`)

// A crease can have identical depth, brightness, and camera-facing component
// on either side. Full normals must still yield one uninterrupted ink line.
for (const size of [42, 64, 132]) {
  const count = size * size, mid = Math.floor(size / 2)
  const light = new Float32Array(count).fill(0.95)
  const depth = new Float32Array(count).fill(1)
  const facing = new Float32Array(count).fill(0.8)
  const normals = new Float32Array(count * 3)
  const surfaces = new Uint8Array(count).fill(1)
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x
    normals[i * 3] = x < mid ? -0.6 : 0.6
    normals[i * 3 + 2] = 0.8
  }
  const edges = Light.featureEdges(depth, normals, surfaces, size, size)
  const ink = Light.stipple(light, depth, facing, size, size, edges, undefined,
    {...Light.defaults(), interiorWidth: 1, interiorOutline: 1, outline: 1})
  for (let y = 2; y < size - 2; y++) {
    check(ink[y * size + mid] >= 0.9, "a bright crease must never be stippled away")
    check(edges[y * size + mid - 1] === 0 && edges[y * size + mid + 1] === 0,
      "a hard edge should be a single grid cell wide")
  }
  // Coplanar surfaces carry no artificial triangulation or material outlines.
  for (let i = 0; i < count; i++) {
    normals[i * 3] = 0; normals[i * 3 + 2] = 1
  }
  check(Light.featureEdges(depth, normals, surfaces, size, size).every(v => v === 0),
    "a flat face should not acquire feature lines")
  // A nearly flush seam remains legible independently of normal changes.
  for (let y = 0; y < size; y++) for (let x = mid; x < size; x++) surfaces[y * size + x] = 2
  const seam = Light.featureEdges(depth, normals, surfaces, size, size)
  for (let y = 2; y < size - 2; y++) {
    check(seam[y * size + mid] >= 0.9 && seam[y * size + mid - 1] === 0,
      "a flush part seam must be continuous and inked on only one side")
  }
  // Background pixels never become geometric edges, even next to a crease.
  depth.fill(-100)
  check(Light.featureEdges(depth, normals, surfaces, size, size).every(v => v === 0),
    "feature edges must never appear in empty space")
}
console.log("Geometric creases, flush seams, flat faces, and empty backgrounds passed at three grid sizes")

// Oblique edges must form one connected contour through row/column changes.
for (const slope of [-2.7, -1, -0.7, -0.3, 0.3, 0.7, 1, 2.7]) {
  const size = 64, depth = new Float32Array(size * size).fill(1)
  const normals = new Float32Array(size * size * 3)
  const surfaces = new Uint8Array(size * size).fill(1)
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x
    normals[i * 3] = x - 32 + slope * (y - 32) > 0 ? 0.6 : -0.6
    normals[i * 3 + 2] = 0.8
  }
  const edges = Light.featureEdges(depth, normals, surfaces, size, size)
  const remaining = new Set()
  for (let y = 2; y < size - 2; y++) for (let x = 2; x < size - 2; x++) {
    if (edges[y * size + x]) remaining.add(y * size + x)
  }
  check(remaining.size > 0, "oblique crease must remain visible")
  const queue = [remaining.values().next().value]
  remaining.delete(queue[0])
  while (queue.length) {
    const i = queue.pop()
    for (const offset of [-size - 1, -size, -size + 1, -1, 1, size - 1, size, size + 1]) {
      if (remaining.delete(i + offset)) queue.push(i + offset)
    }
  }
  check(remaining.size === 0, "oblique crease must be continuous: slope=" + slope)
}

// A hidden crease must not leak through a bright foreground surface.
const plane = {
  shade: 1,
  vertices: [[-1, -1, 1, 0, 0, 1], [1, -1, 1, 0, 0, 1],
    [1, 1, 1, 0, 0, 1], [-1, 1, 1, 0, 0, 1]],
  triangles: [[0, 1, 2], [0, 2, 3]]
}
const hidden = {
  shade: 1,
  vertices: [[-1, -1], [0, -1], [0, 1], [-1, 1], [0, -1], [1, -1], [1, 1], [0, 1]]
    .map((v, i) => [v[0], v[1], 0, i < 4 ? -0.6 : 0.6, 0, 0.8]),
  triangles: [[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]]
}
const occlusionScene = { body: [plane], lid: [], hinge: [0, 0, 0], lidAngle: 0 }
const foreground = Mesh.frame(occlusionScene, "closed", 0, 0, 0, 0, 64, 64, true)
occlusionScene.body.push(hidden)
const occluded = Mesh.frame(occlusionScene, "closed", 0, 0, 0, 0, 64, 64, true)
check(foreground.every((v, i) => v === occluded[i]), "occluded geometry must never create visible ink")
console.log("Eight oblique crease orientations and hidden-surface occlusion passed")

// Soft illumination must leave a planar white shell uniformly pale. The light
// preset (2026-10-19) gives front faces a faint even tint rather than bare
// paper; it must stay uniform and light. A dark material still reads as a
// hole; an actual nearby occluder still casts ink beyond that tint.
{
  const size = 40, count = size * size, scale = size / 3.6
  const light = new Float32Array(count).fill(0.45)
  const depth = new Float32Array(count).fill(1)
  const normals = new Float32Array(count * 3)
  const materials = new Float32Array(count).fill(1)
  for (let i = 0; i < count; i++) normals[i * 3 + 2] = 1
  const flat = Light.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(flat.every(v => v === flat[0] && v < 0.2), "softly lit white planes must remain uniformly pale")
  const center = 20 * size + 20
  materials[center] = 0.1
  const hole = Light.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(hole[center] === 1, "dark material must retain full coverage independently of shell shading")
  materials[center] = 1
  // A raised strip beside the center is nearby contact geometry.
  for (let y = 0; y < size; y++) for (let x = 22; x < size; x++) depth[y * size + x] = 1.12
  const contact = Light.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(contact[center] > flat[center] + 0.04, "nearby geometry must supply localized ambient occlusion")
  check(contact[20 * size + 10] === flat[20 * size + 10], "contact shading must not spread across the flat face")
  // A convex shell falls below its local tangent plane; it should not look dirty.
  depth.fill(0.95); depth[center] = 1
  const convex = Light.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(convex[center] === flat[center], "convex surfaces must not occlude themselves")
}

// A bright diamond makes the old two-cell diagonal stroke easy to detect.
// The outline must stay connected, with no extra ink just inside it.
for (const size of [42, 64, 132]) {
  const depth = new Float32Array(size * size).fill(-100)
  const light = new Float32Array(size * size).fill(0.45)
  const facing = new Float32Array(size * size).fill(1)
  const mid = Math.floor(size / 2), radius = Math.floor(size / 3)
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    if (Math.abs(x - mid) + Math.abs(y - mid) <= radius) depth[y * size + x] = 1
  }
  const ink = Light.stipple(light, depth, facing, size, size)
  const distance = Light.silhouetteDistance(depth, size, size)
  const remaining = new Set()
  for (let i = 0; i < ink.length; i++) {
    check((ink[i] > 0) === (distance[i] === 1), "silhouette must occupy exactly one boundary cell")
    if (ink[i]) remaining.add(i)
  }
  const queue = [remaining.values().next().value]
  remaining.delete(queue[0])
  while (queue.length) {
    const i = queue.pop()
    for (const offset of [-size - 1, -size, -size + 1, -1, 1, size - 1, size, size + 1]) {
      if (remaining.delete(i + offset)) queue.push(i + offset)
    }
  }
  check(remaining.size === 0, "the single-dot silhouette must remain connected")
}
console.log("Clean white faces, material openings, local occlusion, and single-dot silhouettes passed")

// A rounded case wall gets tonal falloff without stippling its forward face.
// Intersect camera rays with a cylinder so depth and normals are consistent.
{
  const size = 96, count = size * size, scale = size / 3.6
  const light = new Float32Array(count).fill(0.45)
  const depth = new Float32Array(count).fill(-100)
  const normals = new Float32Array(count * 3)
  const materials = new Float32Array(count).fill(1)
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const u = (x + 0.5 - size / 2) / scale
    const a = u * u + 36, discriminant = 72 * 72 - 4 * a * (36 - 0.5 * 0.5)
    if (discriminant < 0) continue
    const t = (72 - Math.sqrt(discriminant)) / (2 * a), i = y * size + x
    depth[i] = 6 - 6 * t
    normals[i * 3] = t * u / 0.5
    normals[i * 3 + 2] = depth[i] / 0.5
  }
  const form = Light.surfaceShading(light, depth, normals, materials, size, size, scale)
  let roundedSamples = 0
  for (let y = 10; y < size - 10; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x
    if (depth[i] === -100) continue
    if (normals[i * 3 + 2] > 0.98) check(form[i] < 0.2, "the forward case face must remain pale (the preset's faint front tint)")
    if (form[i] > 0.15 && normals[i * 3 + 2] < 0.70) roundedSamples++
  }
  check(roundedSamples > 100, "the rounded case sides must receive a visible tonal band")

}
console.log("Rounded surfaces preserve clean forward faces")

// Case tone is continuous, not the probability of keeping a dot. Every sample
// in the fade must survive, including values below the old ink-palette floor.
{
  const columns = 40, rows = 12, count = columns * rows
  const light = new Float32Array(count).fill(0.45)
  const depth = new Float32Array(count).fill(1)
  const facing = new Float32Array(count).fill(1)
  const shading = new Float32Array(count)
  for (let y = 0; y < rows; y++) for (let x = 0; x < columns; x++) {
    shading[y * columns + x] = Math.max(0, (30 - x) / 30)
  }
  const ink = Light.stipple(light, depth, facing, columns, rows, null, shading)
  for (let x = 2; x < columns - 2; x++) {
    const i = 6 * columns + x
    check(ink[i] === shading[i], "tonal case samples must not be randomly dropped or darkened")
    check(ink[i] <= ink[i - 1], "the case gradient must fade monotonically into white")
  }
  check(ink[6 * columns + 29] > 0 && ink[6 * columns + 29] < 0.05,
    "faint dots must have no minimum gray intensity")
  check(ink[6 * columns + 31] === 0, "the flat face must resolve to the background")
}
const paintedCase = Mesh.renderFrame(geometry, "open", 1, 0, -0.32, 0.24, 96, 96, true)
check(paintedCase.tonal === true, "all case and lid surfaces must select the tonal palette")
const paintedPods = Mesh.renderFrame(geometry, "", 1, 0, -0.32, 0.24, 96, 96, true)
check(paintedPods.tonal === true, "all earbud surfaces must use the same tonal palette")
check(Mesh.renderFrame(geometry, "open", 1, 0, 0, 0.24, 64, 64, false).tonal === false,
  "dark artwork must retain its illumination palette")
console.log("Continuous gradients and uniform light-theme palette routing passed")

// Matte tone depends only on diffuse incidence, not a reflection half-vector
// or view-dependent curvature. Equal incidence stays equal around a light cone.
{
  const settings = Light.defaults()
  const direction = [settings.lightX, settings.lightY, 0.9539392014 * settings.frontalLight]
  const light = direction.map(v => v / Math.hypot(...direction))
  const planar = Math.hypot(light[0], light[1])
  const tangent = [light[1] / planar, -light[0] / planar, 0]
  const bitangent = [-light[2] * tangent[1], light[2] * tangent[0],
    light[0] * tangent[1] - light[1] * tangent[0]]
  const theta = 0.7
  let reference
  for (let step = 0; step < 72; step++) {
    const phi = step * Math.PI / 36
    const n = light.map((v, i) => Math.cos(theta) * v + Math.sin(theta)
      * (Math.cos(phi) * tangent[i] + Math.sin(phi) * bitangent[i]))
    const tone = Light.matteTone(...n)
    if (reference === undefined) reference = tone
    check(Math.abs(tone - reference) < 1e-9, "equal diffuse incidence must never produce a shiny hotspot")
  }
  // Start from the tone facing the light: the preset may give that a faint
  // tint, which is a starting level, not a step in the falloff.
  let previous = Light.matteTone(...light)
  for (let step = 0; step <= 140; step++) {
    const angle = step / 100
    const n = light.map((v, i) => Math.cos(angle) * v + Math.sin(angle) * tangent[i])
    const tone = Light.matteTone(...n)
    check(tone >= previous - 1e-9, "a surface turning away from the light must darken monotonically")
    check((tone - previous) / settings.surfaceShading < 0.02,
      "matte falloff must remain smooth independently of shadow strength")
    previous = tone
  }
}
console.log("Matte diffuse incidence and smooth volume gradients passed")

// A shallow groove can be less than a cell deep but still needs a soft visible
// depression. Sample its analytic normals at both popup and larger grid sizes.
for (const size of [58, 96, 160]) {
  const count = size * size, scale = size / 3.6
  const depth = new Float32Array(count)
  const normals = new Float32Array(count * 3)
  const center = Math.floor(size / 2), width = 0.08, height = 0.008
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x, worldY = -(y - center) * (1 - 1 / 6) / scale
    const curve = Math.exp(-worldY * worldY / (2 * width * width))
    depth[i] = 1 - height * curve
    const ny = -height * worldY / (width * width) * curve
    const length = Math.hypot(ny, 1)
    normals[i * 3 + 1] = ny / length; normals[i * 3 + 2] = 1 / length
  }
  const relief = Light.recessTone(depth, normals, size, size, scale)
  check(relief[center * size + center] > 0.10, "a shallow concavity must retain readable contrast")
  check(relief[(center + 1) * size + center] > 0.08,
    "recess tone must extend beyond a single dot row")
  for (let offset = 1; offset <= Math.floor(size / 4); offset++) {
    check(relief[(center + offset) * size + center] <= relief[(center + offset - 1) * size + center],
      "a shallow depression must fade smoothly without detached shadow bands")
  }
  check(relief[(center - Math.floor(size / 4)) * size + center] === 0,
    "recess shading must not spread across surrounding white faces")
  // A uniformly tilted plane has no concavity; neither does an outward curve.
  for (let i = 0; i < count; i++) { normals[i * 3 + 1] = 0.1; normals[i * 3 + 2] = Math.sqrt(0.99) }
  depth.fill(1)
  check(Light.recessTone(depth, normals, size, size, scale).every(v => v === 0),
    "a flat seam must not gain an artificial shadow band")
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x, ny = -(y - center) / size
    normals[i * 3 + 1] = ny; normals[i * 3 + 2] = Math.sqrt(1 - ny * ny)
  }
  check(Light.recessTone(depth, normals, size, size, scale).every(v => v === 0),
    "convex walls must not acquire recess shadows")
}
console.log("Shallow recesses, flat seams, and convex surfaces passed at three grid sizes")

// Docked shells use negative space against the case, without erasing case
// recesses, material openings, or contours against empty background.
{
  const size = 9, count = size * size
  const values = new Float32Array(count).fill(0.8)
  const shading = new Float32Array(count).fill(0.4)
  const depth = new Float32Array(count).fill(1)
  const surfaces = new Uint8Array(count).fill(1)
  const materials = new Float32Array(count).fill(1)
  for (let y = 2; y <= 6; y++) for (let x = 2; x <= 6; x++) {
    surfaces[y * size + x] = 3; depth[y * size + x] = 2
  }
  const vent = 4 * size + 4, exposed = 2 * size + 4
  materials[vent] = 0.1; values[vent] = 1
  depth[size + 4] = -100; values[size + 4] = 0
  // A contour belongs at the actual overlap, while a spurious inner crease
  // must resolve to volume tone instead of creating a second outline.
  values[4 * size + 2] = 0; values[4 * size + 3] = 0.94
  const before = values.slice()
  Light.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, size, size)
  check(values[4 * size + 2] > 0.69 && values[4 * size + 3] < 0.3,
    "the overlap must have one crisp boundary followed directly by light volume tone")
  check(values[4 * size + 3] > 0.1 && values[4 * size + 3] < 0.3,
    "docked shells must retain visible light midtones")
  check(values[vent] === 1, "docked vent openings must remain dark")
  check(values[exposed] === before[exposed], "a silhouette against empty background must retain its contour")
  check(values.every((v, i) => surfaces[i] === 3 || v === before[i]),
    "pod contrast must not change the case, its dark recesses, or the background")
  const standalone = before.slice()
  Light.dockedPodContrast(standalone, shading, depth, surfaces, materials, {}, size, size)
  check(standalone.every((v, i) => v === before[i]), "standalone earbuds must retain their shading")
}
console.log("Docked shell contrast, case preservation, vents, and exposed contours passed")

// Keep internal structure distinct from a grazing outer profile. Synthetic
// folds and a material joint exercise the same masks used by the docked mesh.
{
  const size = 19, count = size * size
  const values = new Float32Array(count).fill(0.94)
  const shading = new Float32Array(count).fill(0.3)
  const depth = new Float32Array(count).fill(1)
  const surfaces = new Uint8Array(count).fill(1)
  const materials = new Float32Array(count).fill(1)
  const normals = new Float32Array(count * 3)
  const features = new Float32Array(count).fill(0.94)
  for (let y = 3; y <= 15; y++) for (let x = 3; x <= 15; x++) {
    const i = y * size + x
    surfaces[i] = 3; depth[i] = 2; normals[i * 3 + 2] = 1
  }
  const crease = 8 * size + 8, shadow = 10 * size + 10, rim = 8 * size + 4
  const grazing = 6 * size + 6, joint = 12 * size + 8
  normals[(crease - size) * 3] = 0.8; normals[(crease - size) * 3 + 2] = 0.6
  normals[grazing * 3 + 2] = 0.1
  features[shadow] = 0; shading[shadow] = 0.85
  materials[joint] = 0.86; normals[joint * 3 + 2] = 0.1
  Light.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, size, size, features, normals)
  check(values[6 * size + 12] < 0.25, "smooth shell normals must not become dark dots merely because the feature detector responded")
  check(values[crease] > 0.75, "internal hard edges must retain a readable trace")
  check(values[shadow] > 0.35, "deep internal shadows must survive the pale shell treatment")
  check(values[joint] > 0.75, "silicone-to-shell joins must survive even when the tip turns edge-on")
  check(values[rim] < 0.25 && values[grazing] < 0.25,
    "outer-profile and grazing-angle features must not return as an inset contour")
}
console.log("Internal pod creases, material joins, and deep shadows retain contrast without a profile band")

// Inner silicone is white material, but its inward-facing bore must retain
// deep shading even near the projected tip profile. It must not darken the shell.
{
  const size = 9, count = size * size
  const values = new Float32Array(count).fill(0.02), shading = values.slice()
  const depth = new Float32Array(count).fill(1), surfaces = new Uint8Array(count).fill(1)
  const materials = new Float32Array(count).fill(1), cavities = new Float32Array(count)
  const hole = 4 * size + 4, tip = 4 * size + 3
  surfaces[hole] = surfaces[tip] = 3; depth[hole] = depth[tip] = 2
  materials[hole] = materials[tip] = 0.86; cavities[hole] = 1
  Light.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, size, size, null, null, cavities)
  check(values[hole] > 0.9, "the white silicone bore must keep a dark interior at the profile")
  check(values[tip] > 0.69 && values[tip] < values[hole], "the tip's overlap edge must have a distinct narrow contour")
  const tipMesh = geometry.bud.find(b => b.cavity)
  check(tipMesh && tipMesh.cavity.some(v => v > 0.9) && tipMesh.cavity.some(v => v === 0),
    "the reference tip must distinguish its inner bore from its outer surface")
  const withoutCavity = JSON.parse(JSON.stringify(geometry))
  for (const b of withoutCavity.bud) delete b.cavity
  for (const size of [58, 96, 160]) {
    const args = ["open", 1, 0, -0.6, 0.24, size, size, true]
    const actual = Mesh.frame(geometry, ...args), pale = Mesh.frame(withoutCavity, ...args)
    check(actual.some((v, i) => v - pale[i] > 0.5), "the visible bore must survive rasterization at grid " + size)
  }
}
console.log("Tip overlap contours and depth-tested silicone cavity shading passed")

// Shell gradients need enough distinct painted levels to read as a rounded
// volume, while staying much lighter than the case recesses and dark details.
{
  const columns = 33, rows = 5, count = columns * rows
  const values = new Float32Array(count), shading = new Float32Array(count)
  const depth = new Float32Array(count).fill(1), surfaces = new Uint8Array(count).fill(3)
  const materials = new Float32Array(count).fill(1)
  for (let y = 0; y < rows; y++) for (let x = 0; x < columns; x++) shading[y * columns + x] = x / (columns - 1)
  Light.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, columns, rows)
  const ramp = Array.from(values.slice(2 * columns, 3 * columns))
  check(ramp[0] === 0, "fully lit shell faces must still merge into the background")
  check(new Set(ramp.map(v => Math.round(v * 64))).size >= 16,
    "rounded shells must retain a visible gradient through the painter's 64-level palette")
  check(ramp.every((v, i) => i === 0 || (v >= ramp[i - 1] && v - ramp[i - 1] < 0.03)),
    "shell midtones must progress smoothly without a band or step")
  check(ramp[ramp.length - 1] < 0.4, "shell volume must remain lighter than dark recesses")
}
console.log("Docked pod gradients retain visible, smooth midtones through palette quantization")

// Covering a shaded recess must not remove its neighborhood samples and create
// a bright fringe. Only visibility changes; each exposed surface keeps its tone.
for (const size of [58, 96, 160]) {
  const count = size * size, scale = size / 3.6, center = Math.floor(size / 2)
  const back = {depth: new Float32Array(count), normals: new Float32Array(count * 3),
    materials: new Float32Array(count).fill(Light.defaults().recessShade)}
  const front = {depth: new Float32Array(count).fill(-100), normals: new Float32Array(count * 3), materials: new Float32Array(count).fill(1)}
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x, worldY = (center - y) / scale
    const curve = Math.exp(-worldY * worldY / 0.08)
    back.depth[i] = 1 - 0.08 * curve
    const ny = -2 * 0.08 * worldY / 0.08 * curve, length = Math.hypot(ny, 1)
    back.normals[i * 3 + 1] = ny / length; back.normals[i * 3 + 2] = 1 / length
    front.normals[i * 3 + 2] = 1
  }
  const bare = Light.surfaceShading(null, back.depth, back.normals, back.materials, size, size, scale)
  check(bare.some(v => v > 0.2), "the halo fixture needs a visibly shaded recess")
  const layers = []; layers[1] = back; layers[3] = front
  for (const shift of [-3, 0, 3]) {
    const surfaces = new Uint8Array(count).fill(1)
    front.depth.fill(-100)
    for (let y = center + shift; y < center + size / 4; y++) for (let x = Math.floor(center - size / 6); x < center + size / 6; x++) {
      const i = y * size + x; surfaces[i] = 3; front.depth[i] = 2
    }
    surfaces[0] = 0
    const composed = Light.composeSurfaceShading(layers, [0, 1, 1, 3], surfaces, size, size, scale)
    const frontTone = Light.surfaceShading(null, front.depth, front.normals, front.materials, size, size, scale)
    check(composed.every((v, i) => v === (surfaces[i] === 1 ? bare[i] : surfaces[i] === 3 ? frontTone[i] : 0)),
      "foreground movement must not lighten the exposed recess or reveal hidden shading")
  }
}
console.log("Layered shading preserves recess tone at moving pod boundaries at three grid sizes")

// A smooth bend moving between grid columns must never become a dark stripe.
// Keep a separate sharp step to ensure real creases still receive an outline.
for (const size of [96, 132, 160]) for (const shift of [0, 0.25, 0.5, 0.75]) {
  const count = size * size, middle = Math.floor(size / 2), scale = size / 2.95
  const depth = new Float32Array(count), normals = new Float32Array(count * 3)
  const surfaces = new Uint8Array(count).fill(1)
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x, u = (x - middle - shift) / 2
    const angle = 0.48 * Math.tanh(u)
    depth[i] = 1 - 0.96 / scale * Math.log(Math.cosh(u))
    normals[i * 3] = Math.sin(angle); normals[i * 3 + 2] = Math.cos(angle)
  }
  const broad = Light.featureEdges(depth, normals, surfaces, size, size)
  const strict = Light.featureEdges(depth, normals, surfaces, size, size, true)
  check(broad.some(v => v > 0.9), "the smooth-stem fixture must reproduce the old false stripe")
  check(strict.every(v => v === 0), "smooth moving curvature must remain shading, not a crease line")
  depth.fill(1)
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x
    normals[i * 3] = x < middle ? -0.6 : 0.6; normals[i * 3 + 2] = 0.8
  }
  check(Light.featureEdges(depth, normals, surfaces, size, size, true).some(v => v > 0.9),
    "a genuine hard crease must survive strict standalone-pod detection")
}
console.log("Smooth stem bends stay stripe-free across grid motion while hard creases remain visible")

// Alternating sub-cell depth and duplicate normal responses must not make the
// closed seam switch sides or grow knots at staircase corners.
for (const slope of [-0.3, 0, 0.3]) for (const shift of [0, 0.4, 0.8]) {
  const columns = 40, rows = 30, count = columns * rows
  const depth = new Float32Array(count), surfaces = new Uint8Array(count)
  const edges = new Float32Array(count), seam = []
  for (let x = 0; x < columns; x++) {
    const ySeam = 14 + Math.floor((x - columns / 2) * slope + shift)
    seam[x] = ySeam
    for (let y = 0; y < rows; y++) {
      const i = y * columns + x
      surfaces[i] = y <= ySeam ? 2 : 1
      depth[i] = 1 + (x % 2 ? 0.001 : -0.001) * (surfaces[i] === 1 ? 1 : -1)
      if (Math.abs(y - ySeam) <= 1) edges[i] = 0.94
    }
  }
  const unrelated = (rows - 1) * columns + 20
  edges[unrelated] = 0.82
  Light.closedCaseSeam(edges, depth, surfaces, columns, rows)
  for (let x = 0; x < columns; x++) {
    check(edges[seam[x] * columns + x] > 0.9, "the lid-side seam must remain continuous")
    check(edges[(seam[x] - 1) * columns + x] === 0 && edges[(seam[x] + 1) * columns + x] === 0,
      "closed seam corners must remain one grid cell thick")
    if (x) check(Math.abs(seam[x] - seam[x - 1]) <= 1, "the seam staircase must remain connected")
  }
  check(edges[unrelated] > 0.8, "seam cleanup must leave unrelated geometry intact")
  surfaces.fill(1)
  const before = edges.slice()
  Light.closedCaseSeam(edges, depth, surfaces, columns, rows)
  check(edges.every((v, i) => v === before[i]), "a surface without a lid/body join must remain unchanged")
}
console.log("Closed seams stay connected and single-cell through slopes and depth ties")

// A foreshortened vent must retain width at the popup's coarser grid.
{
  const frame = Mesh.frame(geometry, "open", 1, 0, 0, 0.24, 80, 80, true)
  for (const x of [25, 54]) {
    check(frame[23 * 80 + x] <= 0.025,
      "white pod shell must separate the vent from the dark lid recess")
    check(frame[22 * 80 + x] > 0.65,
      "vent separation must not bleach the lid behind the pod")
    // Vent darkness lightens the vent body as a whole; its shape must survive.
    const vent = Light.defaults().ventInk
    check(frame[24 * 80 + x] > 0.9 * vent && frame[25 * 80 + x] > 0.6 * vent,
      "the vent must have a rounded two-row body, not a diagonal hairline")
    check(frame[26 * 80 + x] < 0.15, "small vents must not grow into broad dark patches")
  }
  const samples = { depth: new Float32Array(144), surfaces: new Uint8Array(144).fill(3),
    materials: new Float32Array(144).fill(1) }
  const triangles = [{surface: 3, vertices: [[4, 5, 0], [8, 5, 0], [8, 5.3, 0]]}]
  const draw = () => Raster.openingFootprints(triangles, samples, 12, 12, 0.65)
  check(draw()[5 * 12 + 6] > 0.2, "sub-cell openings retain partial coverage")
  check(draw()[5 * 12 + 2] === 0, "coverage stays close to the projected opening")
  samples.surfaces.fill(2)
  check(draw().every(v => v === 0), "a lid occluding a vent must hide its entire footprint")
  samples.surfaces.fill(3)
  samples.depth.fill(0.1)
  check(draw().every(v => v === 0), "the pod's own nearer shell must occlude hidden openings")
  samples.surfaces.fill(0)
  check(draw().every(v => v === 0), "opening coverage must never spill into empty background")
}
console.log("Small vent footprints retain capsule width and respect occlusion")

// Either pod follows the same vertical tween, with stable surface identities
// and fixed case/other-pod geometry, whether departing first or second.
for (const left of [false, true]) for (const second of [false, true]) {
  const surface = left ? 3 : 4
  const startState = second ? (left ? "open-left" : "open-right") : "open"
  const endState = second ? "open-empty" : (left ? "open-right" : "open-left")
  const lifts = progress => left ? [second ? 1 : 0, progress] : [progress, second ? 1 : 0]
  const scene = (state, progress) => Mesh.parts(geometry, state, 1, 0, ...lifts(progress))
  const seated = scene(startState, 0), rising = scene(endState, 0.4), absent = scene(endState, 1)
  const fixed = parts => JSON.stringify(parts.filter(p => p.surfaceId !== surface))
  check(fixed(seated) === fixed(rising) && fixed(seated) === fixed(absent),
    "departure must preserve the case and other pod, including their surface identities")
  const pod = seated.find(p => p.surfaceId === surface)
  const lifted = rising.find(p => p.surfaceId === surface)
  check(lifted.at[1] > pod.at[1] && lifted.at[0] === pod.at[0] && lifted.at[2] === pod.at[2],
    "either pod follows a vertical translation from its dock")
  check(JSON.stringify(lifted.angle) === JSON.stringify(pod.angle) && lifted.mesh === pod.mesh,
    "lift retains the actual pod geometry and orientation")
  for (const ink of [false, true]) {
    const frame = (state, progress) => {
      const [rightLift, leftLift] = lifts(progress)
      return Mesh.frame(geometry, state, 1, 0, 0, 0.24, 80, 80, ink, rightLift, 0.98, leftLift)
    }
    const before = frame(startState, 0), start = frame(endState, 0)
    check(before.every((v, i) => v === start[i]), "departure starts at the exact seated rendering")
    const gone = frame(endState, 1), leaving = frame(endState, 0.999)
    check(gone.every((v, i) => v === leaving[i]), "pod must clear the viewport before its mesh is removed")
    check(frame(endState, 0.3).some((v, i) => Math.abs(v - start[i]) > 0.1),
      "the intermediate lift pose must visibly change the frame")
  }
}
console.log("Both pod lifts preserve fixed parts and continuous endpoints, departing first or second")

// Returning pods rotate independently without morphing or exchanging hands.
for (const side of ["left", "right"]) {
  const id = side === "left" ? 3 : 4
  const seated = Mesh.parts(geometry, "open", 1, 0, 0, 0)
  let previousYaw = Infinity
  for (const turn of [1, 0.75, 0.5, 0.25, 0]) {
    const scene = Mesh.parts(geometry, "open", 1, 0, 0, 0, 1, { [side]: turn })
    for (let i = 0; i < scene.length; i++) {
      const a = scene[i], b = seated[i]
      if (a.surfaceId !== id) {
        check(JSON.stringify(a) === JSON.stringify(b), "return rotation moved another part")
        continue
      }
      check(a.mesh === b.mesh && a.mirror === b.mirror && a.scale === b.scale,
        "return must rotate the same physical pod without flattening or mirroring it")
      check(Math.abs(a.angle[1]) <= previousYaw, "return yaw must approach its well continuously")
      if (turn === 0) check(JSON.stringify(a) === JSON.stringify(b), "return must finish at the original seated pose")
    }
    previousYaw = Math.abs(scene.find(part => part.surfaceId === id).angle[1])
  }
}
console.log("Independent pod return rotations preserve handedness and exact seated endpoints")

// The authored recess material darkens both lid sockets and the exposed
// lower well. Compare to identical white geometry so the effect is material,
// not a changed silhouette or stronger lighting on the outer shell.
{
  const white = { ...geometry }
  for (const key of ["body", "lid"]) {
    check(geometry[key].some(b => b.shade === 0.6), "both case parts need the recess material")
    white[key] = geometry[key].map(b => ({ ...b, shade: b.shade === 0.6 ? 1 : b.shade }))
  }
  for (const ink of [false, true]) {
    const draw = (g, state, lid) => Mesh.frame(g, state, lid, 0, 0, 0.24, 80, 80, ink, undefined, 0.98)
    const closed = draw(geometry, "closed", 0), whiteClosed = draw(white, "closed", 0)
    check(closed.every((v, i) => Math.abs(v - whiteClosed[i]) < 1e-6), "recess material must not change the closed shell")
    const actual = draw(geometry, "open-left", 1), neutral = draw(white, "open-left", 1)
    let lidChanges = 0, wellChanges = 0
    for (let i = 0; i < actual.length; i++) {
      const darker = ink ? actual[i] - neutral[i] : neutral[i] - actual[i]
      if (darker > 0.02) {
        if (Math.floor(i / 80) < 25) lidChanges++
        else wellChanges++
      }
    }
    check(lidChanges > 20 && wellChanges > 2, "both recesses must visibly darken in each theme")
  }
}
console.log("Recess materials darken lid sockets and the empty lower well while preserving the closed shell")

// Matte cavities retain soft form shading without a bright specular peak.
// Include the upper inner corner that previously escaped material selection.
{
  for (const key of ["body", "lid"]) {
    check(geometry[key].filter(b => b.shade === 0.6).every(b => b.matte),
      "the recess finish must survive Blender export")
  }
  let count = 0, low = 1, high = 0
  for (const yaw of [-0.15, 0, 0.15]) for (const state of ["open", "open-left"]) {
    const view = { columns: 80, rows: 80, scale: 80 / 3.6 * 0.98, pitch: 0.24, yaw }
    const samples = Raster.rasterize(Mesh.parts(geometry, state, 1, 0), view, { vertexTone: Dark.vertexTone })
    for (let i = 0; i < samples.light.length; i++) {
      if (Math.abs(samples.materials[i] - 0.6) > 1e-6) continue
      const tone = samples.light[i]
      check(tone >= 0.14 && tone < 0.385, "matte recesses must not develop specular or rim-light flares")
      count++; low = Math.min(low, tone); high = Math.max(high, tone)
    }
    if (yaw === 0) {
      const ink = Mesh.frame(geometry, state, 1, 0, yaw, 0.24, 80, 80, true, undefined, 0.98)
      check(Math.abs(samples.materials[16 * 80 + 32] - 0.6) < 1e-6,
        "upper inner socket corner must use recessed plastic")
      for (const x of [37, 42]) for (let y = 19; y <= 24; y++) {
        check(Math.abs(samples.materials[y * 80 + x] - 0.6) < 1e-6,
          "complete medial socket walls must use matte recessed plastic")
      }
      // Front-facing shell takes the preset's faint tint (about 0.11); a crease
      // knot or lost shoulder would be far darker than this cap.
      for (const x of [39, 40]) for (let y = 19; y <= 24; y++) {
        check(samples.materials[y * 80 + x] === 1 && samples.light[y * 80 + x] > 0.6,
          "the central divider must remain bright shell plastic")
        check(ink[y * 80 + x] < 0.2,
          "light rendering must keep the central divider pale instead of adding alternating crease knots")
      }
      for (const y of [17, 18]) for (const x of [38, 39, 40, 41]) {
        check(ink[y * 80 + x] < 0.2,
          "rounded inner socket corners must leave the wider white shoulder visible")
      }
      for (const x of [37, 42]) for (let y = 19; y <= 24; y++) {
        check(ink[y * 80 + x] > 0.7,
          "preserving the white divider must retain the adjacent dark socket walls")
      }
      if (state === "open-left") {
        for (let x = 42; x <= 61; x++) {
          check(ink[33 * 80 + x] > 0.01 && ink[33 * 80 + x] < 0.5,
            "with internal outlines disabled, the lip retains soft shading rather than a solid ink row")
          if (x > 42) check(Math.abs(ink[33 * 80 + x] - ink[33 * 80 + x - 1]) < 0.2,
            "the unoutlined lip must retain a continuous shading gradient")
        }
        for (const y of [31, 32]) {
          check(Math.abs(samples.materials[y * 80 + 55] - 0.6) < 1e-6,
            "the lower well's inner bevel must not leave a glossy sliver near the lip")
        }
        check(samples.materials[30 * 80 + 55] === 1 && samples.light[30 * 80 + 55] > 0.6,
          "the flat case rim above the well must stay bright")
      }
    }
  }
  check(count > 500 && high - low > 0.05, "retain meaningful recessed coverage and curved shading")
}
console.log("Matte lid and lower-well materials retain form shading without specular flares")

if (Deno.args.includes("--update-frames")) {
  Deno.writeTextFileSync(fixturesUrl, "[\n" + fixtures.map(f => "  " + JSON.stringify(f)).join(",\n") + "\n]\n")
  console.log("Updated reference frames after all renderer checks passed")
}
