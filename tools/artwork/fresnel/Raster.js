// Shared geometry projection and triangle coverage on a fixed dot grid.
// A renderer supplies vertexTone(normal, shade, matte) and may request surface
// attributes and complete depth layers grouped by surface ID. This module
// contains no theme, case-state, or earbud-contrast decisions.

function rotate(x, y, z, ax, ay, az) {
  var c = Math.cos(az), s = Math.sin(az), t = x * c - y * s
  y = x * s + y * c; x = t
  c = Math.cos(ax); s = Math.sin(ax); t = y * c - z * s
  z = y * s + z * c; y = t
  c = Math.cos(ay); s = Math.sin(ay); t = x * c + z * s
  z = -x * s + z * c; x = t
  return [x, y, z]
}

function rasterize(scene, view, options) {
  var columns = view.columns, rows = view.rows, scale = view.scale
  var pitch = view.pitch, yaw = view.yaw, captureSurfaces = !!options.surfaceData
  // Larger z is closer to the camera. A dark surface still occludes dots behind it.
  var depth = new Float32Array(columns * rows)
  var light = new Float32Array(columns * rows)
  var facing = captureSurfaces ? new Float32Array(columns * rows) : null
  var normals = captureSurfaces ? new Float32Array(columns * rows * 3) : null
  var surfaces = new Uint8Array(columns * rows)
  var materials = new Float32Array(columns * rows)
  var cavities = captureSurfaces ? new Float32Array(columns * rows) : null
  var matte = captureSurfaces ? new Uint8Array(columns * rows) : null
  // Keep actual surface samples just outside cell centers. Only shallow
  // exterior notches may use them; this is not a dilation of every edge.
  var fringe = { depth: new Float32Array(depth.length), light: new Float32Array(depth.length),
    surfaces: new Uint8Array(depth.length), materials: new Float32Array(depth.length),
    facing: captureSurfaces ? new Float32Array(depth.length) : null,
    normals: captureSurfaces ? new Float32Array(depth.length * 3) : null,
    cavities: captureSurfaces ? new Float32Array(depth.length) : null,
    matte: captureSurfaces ? new Uint8Array(depth.length) : null }
  for (var d = 0; d < depth.length; d++) depth[d] = -100
  for (var d = 0; d < depth.length; d++) fringe.depth[d] = -100
  var contours = [], layers = [], openingTriangles = []
  var shadingGroups = options.groups || []
  for (var p = 0; p < scene.length; p++) {
    var part = scene[p], projected = [], a = part.angle
    var layer = null
    var group = shadingGroups[part.surfaceId]
    if (group !== undefined) {
      if (!layers[group]) {
        layers[group] = { depth: new Float32Array(depth.length),
          normals: new Float32Array(depth.length * 3), materials: new Float32Array(depth.length) }
        for (var d = 0; d < depth.length; d++) layers[group].depth[d] = -100
      }
      layer = layers[group]
    }
    for (var k = 0; k < part.mesh.vertices.length; k++) {
      var v = part.mesh.vertices[k]
      var pos = rotate(v[0] * part.mirror * part.scale, v[1] * part.scale, v[2] * part.scale, a[0], a[1], a[2])
      var n = rotate(v[3] * part.mirror, v[4], v[5], a[0], a[1], a[2])
      pos = [pos[0] + part.at[0], pos[1] + part.at[1], pos[2] + part.at[2]]
      pos = rotate(pos[0], pos[1], pos[2], pitch, yaw, 0)
      n = rotate(n[0], n[1], n[2], pitch, yaw, 0)
      var perspective = 6 / (6 - pos[2])
      var brightness = options.vertexTone(n, part.shade, !!part.mesh.matte)
      projected.push([columns / 2 + pos[0] * scale * perspective + (view.sampleX || 0),
        rows / 2 - pos[1] * scale * perspective + (view.sampleY || 0), pos[2], brightness, n[2], n[0], n[1],
        captureSurfaces && part.mesh.cavity ? part.mesh.cavity[k] : 0])
    }
    if (captureSurfaces && part.mesh.contours) {
      for (var c = 0; c < part.mesh.contours.length; c++) {
        var points = []
        for (var j = 0; j < part.mesh.contours[c].length; j++) points.push(projected[part.mesh.contours[c][j]])
        contours.push({ points: points, surfaceId: part.surfaceId })
      }
    }
    for (var f = 0; f < part.mesh.triangles.length; f++) {
      var face = part.mesh.triangles[f]
      var v0 = projected[face[0]], v1 = projected[face[1]], v2 = projected[face[2]]
      if (v0[4] < 0 && v1[4] < 0 && v2[4] < 0) continue
      var area = (v1[1] - v2[1]) * (v0[0] - v2[0]) + (v2[0] - v1[0]) * (v0[1] - v2[1])
      if (Math.abs(area) < 0.00001) continue
      if (options.openingSurfaces && options.openingSurfaces[part.surfaceId] && part.shade <= 0.2)
        openingTriangles.push({ vertices: [v0, v1, v2], surface: part.surfaceId })
      var altitude0 = Math.abs(area) / Math.hypot(v1[0] - v2[0], v1[1] - v2[1])
      var altitude1 = Math.abs(area) / Math.hypot(v2[0] - v0[0], v2[1] - v0[1])
      var altitude2 = Math.abs(area) / Math.hypot(v0[0] - v1[0], v0[1] - v1[1])
      var x0 = Math.max(0, Math.floor(Math.min(v0[0], v1[0], v2[0])))
      var x1 = Math.min(columns - 1, Math.ceil(Math.max(v0[0], v1[0], v2[0])))
      var y0 = Math.max(0, Math.floor(Math.min(v0[1], v1[1], v2[1])))
      var y1 = Math.min(rows - 1, Math.ceil(Math.max(v0[1], v1[1], v2[1])))
      for (var y = y0; y <= y1; y++) {
        for (var x = x0; x <= x1; x++) {
          var u = ((v1[1] - v2[1]) * (x + 0.5 - v2[0]) + (v2[0] - v1[0]) * (y + 0.5 - v2[1])) / area
          var w = ((v2[1] - v0[1]) * (x + 0.5 - v2[0]) + (v0[0] - v2[0]) * (y + 0.5 - v2[1])) / area
          var t = 1 - u - w
          var outside = u < 0 || w < 0 || t < 0
          if (outside) {
            if (Math.max(-u * altitude0, -w * altitude1, -t * altitude2) > 0.25) continue
            // Clamp to the triangle itself, rather than inventing shading
            // by extrapolating across the silhouette.
            u = Math.max(0, u); w = Math.max(0, w); t = Math.max(0, t)
            var total = u + w + t
            u /= total; w /= total; t /= total
            var nearX = u * v0[0] + w * v1[0] + t * v2[0]
            var nearY = u * v0[1] + w * v1[1] + t * v2[1]
            if (Math.hypot(x + 0.5 - nearX, y + 0.5 - nearY) > 0.25) continue
          }
          var z = u * v0[2] + w * v1[2] + t * v2[2], index = y * columns + x
          if (!outside && layer && z > layer.depth[index]) {
            var lx = u * v0[5] + w * v1[5] + t * v2[5]
            var ly = u * v0[6] + w * v1[6] + t * v2[6]
            var lz = u * v0[4] + w * v1[4] + t * v2[4]
            var ll = Math.sqrt(lx * lx + ly * ly + lz * lz) || 1
            layer.depth[index] = z
            layer.normals[index * 3] = lx / ll
            layer.normals[index * 3 + 1] = ly / ll
            layer.normals[index * 3 + 2] = lz / ll
            layer.materials[index] = part.shade
          }
          var targetDepth = outside ? fringe.depth : depth
          if (z <= targetDepth[index]) continue
          targetDepth[index] = z
          var targetLight = outside ? fringe.light : light
          var targetSurfaces = outside ? fringe.surfaces : surfaces
          var targetMaterials = outside ? fringe.materials : materials
          targetLight[index] = u * v0[3] + w * v1[3] + t * v2[3]
          targetSurfaces[index] = part.surfaceId
          targetMaterials[index] = part.shade
          if (captureSurfaces) {
            var targetMatte = outside ? fringe.matte : matte
            targetMatte[index] = part.mesh.matte ? 1 : 0
            var targetFacing = outside ? fringe.facing : facing
            var targetNormals = outside ? fringe.normals : normals
            var targetCavities = outside ? fringe.cavities : cavities
            targetFacing[index] = u * v0[4] + w * v1[4] + t * v2[4]
            var nx = u * v0[5] + w * v1[5] + t * v2[5]
            var ny = u * v0[6] + w * v1[6] + t * v2[6]
            var nz = targetFacing[index], length = Math.sqrt(nx * nx + ny * ny + nz * nz) || 1
            targetNormals[index * 3] = nx / length
            targetNormals[index * 3 + 1] = ny / length
            targetNormals[index * 3 + 2] = nz / length
            targetCavities[index] = u * v0[7] + w * v1[7] + t * v2[7]
          }
        }
      }
    }
  }
  var samples = { depth: depth, light: light, facing: facing, normals: normals,
    surfaces: surfaces, materials: materials, cavities: cavities, matte: matte,
    contours: contours, layers: layers }
  smoothSilhouette(samples, fringe, shadingGroups, columns, rows)
  if (options.openingSurfaces)
    samples.openingCoverage = openingFootprints(openingTriangles, samples, columns, rows, 0.65)
  return samples
}

// Soft minimum-width coverage for small dark material openings. Work from the
// projected triangles, not neighboring painted dots, to retain rounded ends.
// Final surface/depth tests keep this footprint off occluding parts and air.
function openingFootprints(triangles, samples, columns, rows, radius) {
  var coverage = new Float32Array(columns * rows)
  for (var f = 0; f < triangles.length; f++) {
    var triangle = triangles[f], v = triangle.vertices
    var x0 = Math.max(0, Math.floor(Math.min(v[0][0], v[1][0], v[2][0]) - radius))
    var x1 = Math.min(columns - 1, Math.ceil(Math.max(v[0][0], v[1][0], v[2][0]) + radius))
    var y0 = Math.max(0, Math.floor(Math.min(v[0][1], v[1][1], v[2][1]) - radius))
    var y1 = Math.min(rows - 1, Math.ceil(Math.max(v[0][1], v[1][1], v[2][1]) + radius))
    for (var y = y0; y <= y1; y++) for (var x = x0; x <= x1; x++) {
      var i = y * columns + x
      if (samples.surfaces[i] !== triangle.surface || samples.materials[i] < 0.95) continue
      var distance = radius, z = -100
      for (var e = 0; e < 3; e++) {
        var a = v[e], b = v[(e + 1) % 3]
        var dx = b[0] - a[0], dy = b[1] - a[1], length = dx * dx + dy * dy
        var t = length ? Math.max(0, Math.min(1, ((x + 0.5 - a[0]) * dx + (y + 0.5 - a[1]) * dy) / length)) : 0
        var d = Math.hypot(x + 0.5 - a[0] - t * dx, y + 0.5 - a[1] - t * dy)
        if (d < distance) { distance = d; z = a[2] + t * (b[2] - a[2]) }
      }
      if (z < samples.depth[i] - 0.03) continue
      var weight = 1 - distance / radius
      weight = weight * weight * (3 - 2 * weight)
      coverage[i] = Math.max(coverage[i], weight * 0.85)
    }
  }
  return coverage
}

// A sub-cell reversal can quantize into a conspicuous one-dot divot. Repair
// runs of at most two cells, one cell deep, supported by the same surface on
// three sides and by real geometry within a quarter cell of the sample.
// Enclosed holes, material boundaries, and gaps between objects stay intact.
function smoothSilhouette(samples, fringe, groups, columns, rows) {
  var depth = samples.depth, exterior = new Uint8Array(depth.length)
  var queue = new Int32Array(depth.length), head = 0, tail = 0
  function visit(i) {
    if (depth[i] === -100 && !exterior[i]) { exterior[i] = 1; queue[tail++] = i }
  }
  for (var x = 0; x < columns; x++) { visit(x); visit((rows - 1) * columns + x) }
  for (var y = 0; y < rows; y++) { visit(y * columns); visit(y * columns + columns - 1) }
  while (head < tail) {
    var i = queue[head++], x = i % columns, y = Math.floor(i / columns)
    if (x > 0) visit(i - 1)
    if (x + 1 < columns) visit(i + 1)
    if (y > 0) visit(i - columns)
    if (y + 1 < rows) visit(i + columns)
  }
  function sameSurface(i, j) {
    return depth[j] !== -100 && samples.surfaces[j] === fringe.surfaces[i]
      && samples.materials[j] === fringe.materials[i] && Math.abs(depth[j] - fringe.depth[i]) < 0.12
  }
  function supported(i, step, before, after, cross) {
    var a = 1, b = 1
    while (a <= 2 && a <= before && depth[i - a * step] === -100) a++
    while (b <= 2 && b <= after && depth[i + b * step] === -100) b++
    return a <= before && b <= after && a + b <= 3
      && sameSurface(i, i - a * step) && sameSurface(i, i + b * step)
      && ((sameSurface(i, i + cross) && exterior[i - cross])
        || (sameSurface(i, i - cross) && exterior[i + cross]))
  }
  // Decide from the original coverage; never let a repair grow another repair.
  var repairs = []
  for (var y = 1; y < rows - 1; y++) for (var x = 1; x < columns - 1; x++) {
    var i = y * columns + x
    if (!exterior[i] || fringe.depth[i] === -100) continue
    if (supported(i, 1, x, columns - x - 1, columns)
        || supported(i, columns, y, rows - y - 1, 1)) repairs.push(i)
  }
  for (var r = 0; r < repairs.length; r++) {
    var i = repairs[r]
    depth[i] = fringe.depth[i]
    samples.light[i] = fringe.light[i]
    samples.surfaces[i] = fringe.surfaces[i]
    samples.materials[i] = fringe.materials[i]
    if (samples.matte) samples.matte[i] = fringe.matte[i]
    if (samples.normals) {
      samples.facing[i] = fringe.facing[i]
      samples.cavities[i] = fringe.cavities[i]
      for (var n = 0; n < 3; n++) samples.normals[i * 3 + n] = fringe.normals[i * 3 + n]
      var layer = samples.layers[groups[fringe.surfaces[i]]]
      if (layer && fringe.depth[i] > layer.depth[i]) {
        layer.depth[i] = fringe.depth[i]
        layer.materials[i] = fringe.materials[i]
        for (var n = 0; n < 3; n++) layer.normals[i * 3 + n] = fringe.normals[i * 3 + n]
      }
    }
  }
}
