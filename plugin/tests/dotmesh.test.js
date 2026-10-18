// Run with: deno run --allow-read plugin/tests/dotmesh.test.js
const source = Deno.readTextFileSync(new URL("../DotMesh.js", import.meta.url))
const Mesh = new Function(source + "; return { parts, frame, renderFrame, dockedPodContrast, meshContours, matteTone, recessTone, stipple, featureEdges, surfaceShading, silhouetteDistance }")()
const asset = Deno.readTextFileSync(new URL("../artwork/AirPodsMesh.js", import.meta.url))
const geometry = new Function(asset.replace(/^\.pragma library\s*/, "") + "; return geometry")()
function check(condition, message) { if (!condition) throw new Error(message) }
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
  let traced = Mesh.meshContours([contour], depth, surfaces, size, size)
  for (let x = 5; x <= 10; x++) {
    check(traced.edges[4 * size + x] > 0.8, "the bevel boundary must have an uninterrupted outline")
    check(traced.edges[3 * size + x] === 0 && traced.edges[5 * size + x] === 0,
      "the bevel boundary must be one dot wide")
    check(traced.interior[6 * size + x] === 1 && traced.interior[10 * size + x] === 0,
      "only the bevel interior may soften the part seam")
  }
  for (const samePart of [false, true]) {
    depth.fill(2); surfaces.fill(samePart ? 1 : 2)
    traced = Mesh.meshContours([contour], depth, surfaces, size, size)
    check(traced.edges.every(v => v === 0) && traced.interior.every(v => v === 0),
      "hidden bevels must neither draw nor erase foreground edges")
  }
  depth.fill(1); surfaces.fill(1)
  for (const point of contour.points) point[4] = -1
  traced = Mesh.meshContours([contour], depth, surfaces, size, size)
  check(traced.edges.every(v => v === 0) && traced.interior.every(v => v === 0),
    "back-facing bevels must not appear on the back of the case")
}
console.log("Mesh bevel outlines, interior coverage, and occlusion passed")

check(triangles < 7000, "runtime asset exceeded its stored triangle budget")
check(geometry.bud.some(batch => batch.shade <= 0.1), "earbud vents/openings must survive export")

function checkSilhouette(light, ink, size, label) {
  for (let y = 1; y < size - 1; y++) for (let x = 1; x < size - 1; x++) {
    const i = y * size + x
    if (!light[i]) continue
    const exposed = [-size, -1, 1, size]
      .some(offset => light[i + offset] === 0)
    if (exposed) check(ink[i] >= 0.9, `the silhouette must never lose an edge dot: ${label} at ${x},${y}`)
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
    checkSilhouette(light, ink, 64, state + " yaw=" + yaw)
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
    checkSilhouette(light, ink, size, `${state} grid=${size} yaw=${yaw}`)
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
const bright = Mesh.stipple(light, depth, facing, 9, 9)
check(bright.some(v => v > 0), "the silhouette remains visible on a bright surface")
for (let y = 2; y < 7; y++) for (let x = 2; x < 7; x++) {
  check(bright[y * 9 + x] === 0, "bright interior must have no dots")
}
for (let y = 3; y < 6; y++) for (let x = 3; x < 6; x++) light[y * 9 + x] = 0.03
const recess = Mesh.stipple(light, depth, facing, 9, 9)
for (let y = 3; y < 6; y++) for (let x = 3; x < 6; x++) {
  check(recess[y * 9 + x] > 0.9, "ear openings and vents retain strong ink coverage")
}
check(recess.every((v, i) => depth[i] !== -100 || v === 0), "empty background stays empty")
const repeated = Mesh.stipple(light, depth, facing, 9, 9)
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
  const edges = Mesh.featureEdges(depth, normals, surfaces, size, size)
  const ink = Mesh.stipple(light, depth, facing, size, size, edges)
  for (let y = 2; y < size - 2; y++) {
    check(ink[y * size + mid] >= 0.9, "a bright crease must never be stippled away")
    check(edges[y * size + mid - 1] === 0 && edges[y * size + mid + 1] === 0,
      "a hard edge should be a single grid cell wide")
  }
  // Coplanar surfaces carry no artificial triangulation or material outlines.
  for (let i = 0; i < count; i++) {
    normals[i * 3] = 0; normals[i * 3 + 2] = 1
  }
  check(Mesh.featureEdges(depth, normals, surfaces, size, size).every(v => v === 0),
    "a flat face should not acquire feature lines")
  // A nearly flush seam remains legible independently of normal changes.
  for (let y = 0; y < size; y++) for (let x = mid; x < size; x++) surfaces[y * size + x] = 2
  const seam = Mesh.featureEdges(depth, normals, surfaces, size, size)
  for (let y = 2; y < size - 2; y++) {
    check(seam[y * size + mid] >= 0.9 && seam[y * size + mid - 1] === 0,
      "a flush part seam must be continuous and inked on only one side")
  }
  // Background pixels never become geometric edges, even next to a crease.
  depth.fill(-100)
  check(Mesh.featureEdges(depth, normals, surfaces, size, size).every(v => v === 0),
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
  const edges = Mesh.featureEdges(depth, normals, surfaces, size, size)
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

// Soft illumination must leave a planar white shell completely clean. A dark
// material still reads as a hole; an actual nearby occluder still casts ink.
{
  const size = 40, count = size * size, scale = size / 3.6
  const light = new Float32Array(count).fill(0.45)
  const depth = new Float32Array(count).fill(1)
  const normals = new Float32Array(count * 3)
  const materials = new Float32Array(count).fill(1)
  for (let i = 0; i < count; i++) normals[i * 3 + 2] = 1
  const flat = Mesh.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(flat.every(v => v === 0), "softly lit white planes must have zero stipple coverage")
  const center = 20 * size + 20
  materials[center] = 0.1
  const hole = Mesh.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(hole[center] === 1, "dark material must retain full coverage independently of shell shading")
  materials[center] = 1
  // A raised strip beside the center is nearby contact geometry.
  for (let y = 0; y < size; y++) for (let x = 22; x < size; x++) depth[y * size + x] = 1.12
  const contact = Mesh.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(contact[center] > 0.04, "nearby geometry must supply localized ambient occlusion")
  check(contact[20 * size + 10] === 0, "contact shading must not spread across the flat face")
  // A convex shell falls below its local tangent plane; it should not look dirty.
  depth.fill(0.95); depth[center] = 1
  const convex = Mesh.surfaceShading(light, depth, normals, materials, size, size, scale)
  check(convex[center] === 0, "convex surfaces must not occlude themselves")
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
  const ink = Mesh.stipple(light, depth, facing, size, size)
  const distance = Mesh.silhouetteDistance(depth, size, size)
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
  const form = Mesh.surfaceShading(light, depth, normals, materials, size, size, scale)
  let roundedSamples = 0
  for (let y = 10; y < size - 10; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x
    if (depth[i] === -100) continue
    if (normals[i * 3 + 2] > 0.98) check(form[i] === 0, "the forward case face must remain white")
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
  const ink = Mesh.stipple(light, depth, facing, columns, rows, null, shading)
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
  const light = [-0.18, 0.24, 0.9539392014]
  const tangent = [0.8, 0.6, 0]
  const bitangent = [-light[2] * 0.6, light[2] * 0.8, -0.3]
  const theta = 0.7
  let reference
  for (let step = 0; step < 72; step++) {
    const phi = step * Math.PI / 36
    const n = light.map((v, i) => Math.cos(theta) * v + Math.sin(theta)
      * (Math.cos(phi) * tangent[i] + Math.sin(phi) * bitangent[i]))
    const tone = Mesh.matteTone(...n)
    if (reference === undefined) reference = tone
    check(Math.abs(tone - reference) < 1e-9, "equal diffuse incidence must never produce a shiny hotspot")
  }
  let previous = 0
  for (let step = 0; step <= 140; step++) {
    const angle = step / 100
    const n = light.map((v, i) => Math.cos(angle) * v + Math.sin(angle) * tangent[i])
    const tone = Mesh.matteTone(...n)
    check(tone >= previous - 1e-9, "a surface turning away from the light must darken monotonically")
    check(tone - previous < 0.02, "matte shading must not form narrow tonal bands")
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
  const relief = Mesh.recessTone(depth, normals, size, size, scale)
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
  check(Mesh.recessTone(depth, normals, size, size, scale).every(v => v === 0),
    "a flat seam must not gain an artificial shadow band")
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const i = y * size + x, ny = -(y - center) / size
    normals[i * 3 + 1] = ny; normals[i * 3 + 2] = Math.sqrt(1 - ny * ny)
  }
  check(Mesh.recessTone(depth, normals, size, size, scale).every(v => v === 0),
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
  // Simulate an enforced inner crease beside a white overlap edge. Neither
  // should survive as a band when the underlying volume has uniform tone.
  values[4 * size + 2] = 0; values[4 * size + 3] = 0.94
  const before = values.slice()
  Mesh.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, size, size)
  check(values[4 * size + 2] === values[4 * size + 3],
    "shell tone must reach the overlap boundary without a white rim or inset contour")
  check(values[4 * size + 3] > 0 && values[4 * size + 3] < 0.04,
    "docked shells must retain faint continuous volume shading")
  check(values[vent] === 1, "docked vent openings must remain dark")
  check(values[exposed] === before[exposed], "a silhouette against empty background must retain its contour")
  check(values.every((v, i) => surfaces[i] === 3 || v === before[i]),
    "pod contrast must not change the case, its dark recesses, or the background")
  const standalone = before.slice()
  Mesh.dockedPodContrast(standalone, shading, depth, surfaces, materials, {}, size, size)
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
  normals[grazing * 3 + 2] = 0.1
  features[shadow] = 0; shading[shadow] = 0.85
  materials[joint] = 0.86; normals[joint * 3 + 2] = 0.1
  Mesh.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, size, size, features, normals)
  check(values[crease] > 0.75, "internal hard edges must retain a readable trace")
  check(values[shadow] > 0.35, "deep internal shadows must survive the pale shell treatment")
  check(values[joint] > 0.75, "silicone-to-shell joins must survive even when the tip turns edge-on")
  check(values[rim] < 0.04 && values[grazing] < 0.04,
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
  Mesh.dockedPodContrast(values, shading, depth, surfaces, materials, {3: true}, size, size, null, null, cavities)
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
