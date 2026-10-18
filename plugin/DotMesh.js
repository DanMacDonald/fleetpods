// Small surface renderer for the artwork study. Geometry stays in object space;
// triangles are depth-tested on a fixed dot grid, like a terminal character grid.
// No textures, frame assets, or random particles are involved.

function rotate(x, y, z, ax, ay, az) {
  var c = Math.cos(az), s = Math.sin(az), t = x * c - y * s
  y = x * s + y * c; x = t
  c = Math.cos(ax); s = Math.sin(ax); t = y * c - z * s
  z = y * s + z * c; y = t
  c = Math.cos(ay); s = Math.sin(ay); t = x * c + z * s
  z = -x * s + z * c; x = t
  return [x, y, z]
}

function parts(geometry, state, lid, time) {
  var result = [], surfaceId = 0
  function add(batches, at, angle, mirror, scale, dockedPod) {
    surfaceId++
    for (var i = 0; i < batches.length; i++) {
      result.push({ mesh: batches[i], at: at, angle: angle,
        surfaceId: surfaceId, shade: batches[i].shade, mirror: mirror || 1, scale: scale || 1,
        dockedPod: !!dockedPod })
    }
  }
  function bud(side, docked) {
    if (docked) {
      add(geometry.bud, [side * geometry.dock[0], geometry.dock[1], geometry.dock[2]],
        [geometry.dockAngle, 0, 0], side, 1, true)
    } else {
      add(geometry.bud, [side * 0.70, side * -0.16 + Math.sin(time + side) * 0.025, 0],
        [geometry.dockAngle + 0.10, side * 0.42, side * 0.20], side, 1.2)
    }
  }
  if (state === "") {
    bud(-1, false)
    bud(1, false)
  } else {
    add(geometry.body, [0, 0, 0], [0, 0, 0])
    add(geometry.lid, geometry.hinge, [lid * geometry.lidAngle, 0, 0])
    if (lid > 0.03) {
      if (state !== "open-right" && state !== "open-empty") bud(-1, true)
      if (state !== "open-left" && state !== "open-empty") bud(1, true)
    }
  }
  return result
}

// Numeric grid for geometry checks and consumers that do not need the palette.
function frame(geometry, state, lid, time, yaw, pitch, columns, rows, ink) {
  return renderFrame(geometry, state, lid, time, yaw, pitch, columns, rows, ink).values
}

// All light-theme surfaces use tone against the background. Dark-theme
// surfaces retain the illuminated palette.
function renderFrame(geometry, state, lid, time, yaw, pitch, columns, rows, ink) {
  // Larger z is closer to the camera. A dark surface still occludes dots behind it.
  var depth = new Float32Array(columns * rows)
  var light = new Float32Array(columns * rows)
  var facing = ink ? new Float32Array(columns * rows) : null
  var normals = ink ? new Float32Array(columns * rows * 3) : null
  var surfaces = ink ? new Uint8Array(columns * rows) : null
  var materials = ink ? new Float32Array(columns * rows) : null
  var cavities = ink ? new Float32Array(columns * rows) : null
  for (var d = 0; d < depth.length; d++) depth[d] = -100
  var scene = parts(geometry, state, lid, time)
  var contours = []
  var dockedPods = {}
  var scale = Math.min(columns, rows) / (state === "" ? 2.95 : 3.6)
  for (var p = 0; p < scene.length; p++) {
    var part = scene[p], projected = [], a = part.angle
    if (part.dockedPod) dockedPods[part.surfaceId] = true
    for (var k = 0; k < part.mesh.vertices.length; k++) {
      var v = part.mesh.vertices[k]
      var pos = rotate(v[0] * part.mirror * part.scale, v[1] * part.scale, v[2] * part.scale, a[0], a[1], a[2])
      var n = rotate(v[3] * part.mirror, v[4], v[5], a[0], a[1], a[2])
      pos = [pos[0] + part.at[0], pos[1] + part.at[1], pos[2] + part.at[2]]
      pos = rotate(pos[0], pos[1], pos[2], pitch, yaw, 0)
      n = rotate(n[0], n[1], n[2], pitch, yaw, 0)
      var perspective = 6 / (6 - pos[2])
      var diffuse = Math.max(0, -0.45 * n[0] + 0.65 * n[1] + 0.61 * n[2])
      var brightness
      if (ink) {
        // Matte diffuse illumination only. Specular and rim-light terms belong
        // exclusively to the existing dark-theme treatment.
        brightness = Math.min(1, (0.20 + 0.80 * diffuse) * part.shade)
      } else {
        var rim = Math.pow(1 - Math.abs(n[2]), 3) * 0.10
        var highlight = Math.pow(Math.max(0, -0.25 * n[0] + 0.36 * n[1] + 0.90 * n[2]), 24)
        brightness = Math.min(1, (0.08 + 0.77 * diffuse + 0.28 * highlight + rim) * part.shade)
      }
      projected.push([columns / 2 + pos[0] * scale * perspective,
        rows / 2 - pos[1] * scale * perspective, pos[2], brightness, n[2], n[0], n[1],
        ink && part.mesh.cavity ? part.mesh.cavity[k] : 0])
    }
    if (ink && part.mesh.contours) {
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
      var x0 = Math.max(0, Math.floor(Math.min(v0[0], v1[0], v2[0])))
      var x1 = Math.min(columns - 1, Math.ceil(Math.max(v0[0], v1[0], v2[0])))
      var y0 = Math.max(0, Math.floor(Math.min(v0[1], v1[1], v2[1])))
      var y1 = Math.min(rows - 1, Math.ceil(Math.max(v0[1], v1[1], v2[1])))
      for (var y = y0; y <= y1; y++) {
        for (var x = x0; x <= x1; x++) {
          var u = ((v1[1] - v2[1]) * (x + 0.5 - v2[0]) + (v2[0] - v1[0]) * (y + 0.5 - v2[1])) / area
          var w = ((v2[1] - v0[1]) * (x + 0.5 - v2[0]) + (v0[0] - v2[0]) * (y + 0.5 - v2[1])) / area
          var t = 1 - u - w
          if (u < 0 || w < 0 || t < 0) continue
          var z = u * v0[2] + w * v1[2] + t * v2[2], index = y * columns + x
          if (z <= depth[index]) continue
          depth[index] = z
          light[index] = u * v0[3] + w * v1[3] + t * v2[3]
          if (ink) {
            facing[index] = u * v0[4] + w * v1[4] + t * v2[4]
            var nx = u * v0[5] + w * v1[5] + t * v2[5]
            var ny = u * v0[6] + w * v1[6] + t * v2[6]
            var nz = facing[index], length = Math.sqrt(nx * nx + ny * ny + nz * nz) || 1
            normals[index * 3] = nx / length
            normals[index * 3 + 1] = ny / length
            normals[index * 3 + 2] = nz / length
            surfaces[index] = part.surfaceId
            materials[index] = part.shade
            cavities[index] = u * v0[7] + w * v1[7] + t * v2[7]
          }
        }
      }
    }
  }
  if (!ink) return { values: light, tonal: false }
  var edges = featureEdges(depth, normals, surfaces, columns, rows)
  var traced = meshContours(contours, depth, surfaces, columns, rows)
  // Inside the finger bevel, the part join is a soft contact shadow. Its two
  // outer boundaries carry the outline instead of a third solid center line.
  for (var i = 0; i < edges.length; i++) edges[i] = Math.max(traced.interior[i] ? 0 : edges[i], traced.edges[i])
  var shading = surfaceShading(light, depth, normals, materials, columns, rows, scale)
  var values = stipple(light, depth, facing, columns, rows, edges, shading)
  dockedPodContrast(values, shading, depth, surfaces, materials, dockedPods, columns, rows, edges, normals, cavities)
  return { values: values, tonal: true }
}

// In the open case, pale shells read against the shaded sockets by their white
// silhouette. Pale volume tone reaches the overlap boundary without an inset
// profile line. Restore true material joins, interior creases, and deep shadows
// separately, so internal structure remains legible. Exposed silhouettes and
// dark openings retain ink.
function dockedPodContrast(values, shading, depth, surfaces, materials, dockedPods, columns, rows, features, normals, cavities) {
  for (var y = 0; y < rows; y++) for (var x = 0; x < columns; x++) {
    var i = y * columns + x
    if (!dockedPods[surfaces[i]] || materials[i] <= 0.2) continue
    var neighbors = [x > 0 ? i - 1 : i, x + 1 < columns ? i + 1 : i,
      y > 0 ? i - columns : i, y + 1 < rows ? i + columns : i]
    var exposed = false, joint = false, tipBoundary = false
    for (var k = 0; k < neighbors.length; k++) {
      var j = neighbors[k]
      if (depth[j] === -100) exposed = true
      // Silicone-to-shell joins are actual internal boundaries, including
      // where a tip turns edge-on. Trace just the lower-shade material side.
      if (surfaces[j] === surfaces[i] && materials[j] > materials[i] + 0.04) joint = true
      if (materials[i] > 0.8 && materials[i] < 0.9 && depth[j] !== -100
          && surfaces[j] !== surfaces[i] && depth[j] < depth[i]) tipBoundary = true
    }
    if (exposed) continue
    var pale = Math.pow(shading[i], 1.5) * 0.08
    // Internal folds need ink, but grazing shell normals and part-overlap
    // edges must stay pale. Check both sides beyond the edge detector's
    // footprint so an outer profile cannot reappear as an inset outline.
    var internal = normals && normals[i * 3 + 2] > 0.45
    for (var dy = -2; internal && dy <= 2; dy++) {
      for (var dx = -2; dx <= 2; dx++) {
        var sx = x + dx, sy = y + dy
        if (sx < 0 || sx >= columns || sy < 0 || sy >= rows
            || surfaces[sy * columns + sx] !== surfaces[i]) { internal = false; break }
      }
    }
    var detail = joint ? 0.8 : tipBoundary ? 0.7 : internal && features ? features[i] * 0.85 : 0
    var shadow = internal ? Math.pow(Math.max(0, (shading[i] - 0.45) / 0.55), 1.5) * 0.65 : 0
    var cavity = cavities ? Math.pow(cavities[i], 0.7) * 0.95 : 0
    values[i] = Math.max(pale, detail, shadow, cavity)
  }
}

// Reference-mesh landmarks retain shallow bevels that normal thresholds miss.
// Rasterize each boundary as a one-cell staircase, then test the final visible
// surface so contours cannot show through the case, lid, or earbuds.
function meshContours(contours, depth, surfaces, columns, rows) {
  var result = new Float32Array(depth.length)
  var interior = new Uint8Array(depth.length)
  for (var c = 0; c < contours.length; c++) {
    var contour = contours[c]
    // Each boundary and its endpoint chord enclose one half of the bevel.
    // A triangle fan supplies the same projected, depth-tested footprint as
    // the mesh; it is not a screen-space decal or a closed-case-only overlay.
    for (var p = 2; p < contour.points.length; p++) {
      var a = contour.points[0], b = contour.points[p - 1], d = contour.points[p]
      var area = (b[1] - d[1]) * (a[0] - d[0]) + (d[0] - b[0]) * (a[1] - d[1])
      if (Math.abs(area) < 0.00001) continue
      var x0 = Math.max(0, Math.floor(Math.min(a[0], b[0], d[0])))
      var x1 = Math.min(columns - 1, Math.ceil(Math.max(a[0], b[0], d[0])))
      var y0 = Math.max(0, Math.floor(Math.min(a[1], b[1], d[1])))
      var y1 = Math.min(rows - 1, Math.ceil(Math.max(a[1], b[1], d[1])))
      for (var y = y0; y <= y1; y++) for (var x = x0; x <= x1; x++) {
        var u = ((b[1] - d[1]) * (x + 0.5 - d[0]) + (d[0] - b[0]) * (y + 0.5 - d[1])) / area
        var v = ((d[1] - a[1]) * (x + 0.5 - d[0]) + (a[0] - d[0]) * (y + 0.5 - d[1])) / area
        var w = 1 - u - v, i = y * columns + x
        if (u < 0 || v < 0 || w < 0 || surfaces[i] !== contour.surfaceId || depth[i] === -100) continue
        if (u * a[4] + v * b[4] + w * d[4] > 0.05
            && depth[i] <= u * a[2] + v * b[2] + w * d[2] + 0.012) interior[i] = 1
      }
    }
    for (var p = 1; p < contour.points.length; p++) {
      var a = contour.points[p - 1], b = contour.points[p]
      var x = Math.floor(a[0]), y = Math.floor(a[1])
      var endX = Math.floor(b[0]), endY = Math.floor(b[1])
      var dx = Math.abs(endX - x), dy = -Math.abs(endY - y)
      var sx = x < endX ? 1 : -1, sy = y < endY ? 1 : -1, error = dx + dy
      var vx = b[0] - a[0], vy = b[1] - a[1], length2 = vx * vx + vy * vy
      var bias = 0.012 + Math.abs(b[2] - a[2]) / Math.max(1, Math.sqrt(length2))
      while (true) {
        var t = length2 ? Math.max(0, Math.min(1, ((x + 0.5 - a[0]) * vx + (y + 0.5 - a[1]) * vy) / length2)) : 0
        var z = a[2] + t * (b[2] - a[2]), nz = a[4] + t * (b[4] - a[4])
        var i = y * columns + x
        if (x >= 0 && x < columns && y >= 0 && y < rows && nz > 0.05
            && surfaces[i] === contour.surfaceId && depth[i] !== -100 && depth[i] - z <= bias) result[i] = 0.82
        if (x === endX && y === endY) break
        var twice = 2 * error
        if (twice >= dy) { error += dy; x += sx }
        if (twice <= dx) { error += dx; y += sy }
      }
    }
  }
  return { edges: result, interior: interior }
}

// Surface-normal changes reveal creases even when their depth and brightness
// match. Suppress neighboring responses across the bend to keep a narrow line;
// never trace triangle boundaries or rely on the light direction.
function featureEdges(depth, normals, surfaces, columns, rows) {
  // A finer lattice measures the same bend over a shorter surface distance.
  var resolution = Math.max(1, Math.min(columns, rows) / 64)
  var threshold = 0.18 / (resolution * resolution)
  var horizontal = new Float32Array(depth.length)
  var vertical = new Float32Array(depth.length)
  var edges = new Float32Array(depth.length)
  function bend(a, b) {
    if (depth[a] === -100 || depth[b] === -100 || surfaces[a] !== surfaces[b]) return 0
    return Math.max(0, 1 - normals[a * 3] * normals[b * 3]
      - normals[a * 3 + 1] * normals[b * 3 + 1]
      - normals[a * 3 + 2] * normals[b * 3 + 2])
  }
  for (var y = 1; y < rows - 1; y++) {
    for (var x = 1; x < columns - 1; x++) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      horizontal[i] = bend(i - 1, i + 1)
      vertical[i] = bend(i - columns, i + columns)
      // Parts can meet almost flush (the closed lid seam). Trace only the
      // foreground side, with a stable tie break for exactly matching depths.
      var neighbors = [i - 1, i + 1, i - columns, i + columns]
      for (var k = 0; k < neighbors.length; k++) {
        var j = neighbors[k]
        if (depth[j] !== -100 && surfaces[i] !== surfaces[j]
            && (depth[i] > depth[j] || (depth[i] === depth[j] && surfaces[i] > surfaces[j]))) edges[i] = 0.94
      }
    }
  }
  for (var y = 1; y < rows - 1; y++) {
    for (var x = 1; x < columns - 1; x++) {
      var i = y * columns + x
      // Thin each direction independently. Choosing only the stronger axis
      // creates gaps where an oblique edge switches between rows and columns.
      if ((horizontal[i] > threshold && horizontal[i] >= horizontal[i - 1]
          && horizontal[i] > horizontal[i + 1])
          || (vertical[i] > threshold && vertical[i] >= vertical[i - columns]
          && vertical[i] > vertical[i + columns])) edges[i] = 0.94
    }
  }
  return edges
}

// A fixed broad frontal light describes matte volumes. The white shoulder
// keeps well-lit faces bare; there is no specular lobe or curvature gate that
// can turn a smooth surface into disconnected bright bands.
function matteTone(nx, ny, nz) {
  var diffuse = Math.max(0, -0.18 * nx + 0.24 * ny + 0.9539392014 * nz)
  var shadow = Math.max(0, Math.min(1, (0.84 - diffuse) / 0.74))
  return Math.pow(shadow, 1.25) * 0.85
}

// Shallow bevels can carry relief in their normals even when their depth
// difference is smaller than one grid cell. Only inward-bending normals add
// tone; flat and convex surfaces remain unchanged. Integrating nearby radii
// keeps shallow relief visible without separate rings at each sample distance.
function recessTone(depth, normals, columns, rows, scale) {
  var tones = new Float32Array(depth.length)
  var radius = Math.max(1, Math.round(scale * 0.10))
  function nearby(i, j) {
    // A sharp ledge is a separate face, not the wall of a shallow depression.
    return depth[j] !== -100 && Math.abs(depth[i] - depth[j]) < 0.16
      && normals[i * 3] * normals[j * 3] + normals[i * 3 + 1] * normals[j * 3 + 1]
        + normals[i * 3 + 2] * normals[j * 3 + 2] > 0.75
  }
  for (var y = radius; y < rows - radius; y++) {
    for (var x = radius; x < columns - radius; x++) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      var bend = 0, weight = 0
      for (var r = 1; r <= radius; r++) {
        var left = i - r, right = i + r
        var top = i - r * columns, bottom = i + r * columns
        var w = 1 - r / (radius + 1)
        var curve = 0
        if (nearby(i, left) && nearby(i, right)) curve += normals[left * 3] - normals[right * 3]
        if (nearby(i, top) && nearby(i, bottom)) curve += normals[bottom * 3 + 1] - normals[top * 3 + 1]
        bend += Math.max(0, curve - 0.035) * w
        weight += w
      }
      tones[i] = Math.min(0.65, bend / weight * 3.2)
    }
  }
  // Filter the cavity response over adjacent surface samples. A narrow bevel
  // needs a continuous falloff, not isolated rows at the sampling radii.
  var softened = new Float32Array(depth.length)
  for (var y = 1; y < rows - 1; y++) {
    for (var x = 1; x < columns - 1; x++) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      var total = 0, weight = 0
      for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
          var j = i + dy * columns + dx
          if (!nearby(i, j)) continue
          var w = (dx === 0 ? 2 : 1) * (dy === 0 ? 2 : 1)
          total += tones[j] * w
          weight += w
        }
      }
      softened[i] = total / weight
    }
  }
  return softened
}

// Local occlusion comes from visible geometry rising above the surface's
// tangent plane. Convex shells and flat faces do not occlude themselves.
// This is a small screen-space approximation, not a baked lighting texture.
function surfaceShading(light, depth, normals, materials, columns, rows, scale) {
  var density = new Float32Array(depth.length)
  var relief = recessTone(depth, normals, columns, rows, scale)
  var radius = Math.max(2, Math.round(scale * 0.18))
  var reach = 0.50
  var directions = [[1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0], [-1, -1], [0, -1], [1, -1]]
  for (var y = 0; y < rows; y++) {
    for (var x = 0; x < columns; x++) {
      var i = y * columns + x, z = depth[i]
      if (z === -100) continue
      // Vents and holes are dark materials, not dense shell shading.
      if (materials[i] <= 0.2) { density[i] = 1; continue }
      var nx = normals[i * 3], ny = normals[i * 3 + 1], nz = normals[i * 3 + 2]
      var step = (1 - z / 6) / scale
      var px = (x + 0.5 - columns / 2) * step
      var py = (rows / 2 - y - 0.5) * step
      var occlusion = 0
      for (var k = 0; k < directions.length; k++) {
        var sx = x + directions[k][0] * radius, sy = y + directions[k][1] * radius
        if (sx < 0 || sx >= columns || sy < 0 || sy >= rows) continue
        var j = sy * columns + sx, sampleZ = depth[j]
        if (sampleZ === -100) continue
        var sampleStep = (1 - sampleZ / 6) / scale
        var dx = (sx + 0.5 - columns / 2) * sampleStep - px
        var dy = (rows / 2 - sy - 0.5) * sampleStep - py
        var dz = sampleZ - z, distance = Math.sqrt(dx * dx + dy * dy + dz * dz)
        // Ignore remote overlapping surfaces and numerical deviations from a
        // plane. Nearby concave rims and contacts supply the soft ink falloff.
        if (distance > reach || distance < 0.0001) continue
        occlusion += Math.max(0, (nx * dx + ny * dy + nz * dz) / distance - 0.12)
          * Math.max(0, 1 - distance / reach)
      }
      var ambient = Math.max(0, occlusion / directions.length * 3.0 - 0.035)
      var tone = matteTone(nx, ny, nz)
      // Blend contact shadow smoothly instead of selecting between competing
      // shading fields, which can leave seams across an otherwise smooth face.
      density[i] = Math.min(0.9, tone + (ambient * 0.65 + relief[i]) * (1 - tone))
    }
  }
  return density
}

// Cardinal adjacency gives a single-cell silhouette, including a connected
// staircase on diagonals. Distance also prevents a second geometric outline
// or a dense grazing-angle band from thickening that silhouette.
function silhouetteDistance(depth, columns, rows) {
  var distance = new Uint8Array(depth.length)
  for (var y = 0; y < rows; y++) {
    for (var x = 0; x < columns; x++) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      distance[i] = Math.min(x === 0 ? 1 : distance[i - 1] + 1,
        y === 0 ? 1 : distance[i - columns] + 1, 4)
    }
  }
  for (var y = rows - 1; y >= 0; y--) {
    for (var x = columns - 1; x >= 0; x--) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      distance[i] = Math.min(distance[i], x === columns - 1 ? 1 : distance[i + 1] + 1,
        y === rows - 1 ? 1 : distance[i + columns] + 1)
    }
  }
  return distance
}

// Every visible surface uses continuous tone, including the darkest vents.
// White faces merge into the background; silhouettes remain one-dot contours.
function stipple(light, depth, facing, columns, rows, features, shading) {
  var dots = new Float32Array(light.length)
  var boundary = silhouetteDistance(depth, columns, rows)
  for (var y = 0; y < rows; y++) {
    for (var x = 0; x < columns; x++) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      var edge = boundary[i] === 1 ? 1 : 0
      if (boundary[i] > 2 && features) edge = features[i]
      // Keep foreground lips, but don't mistake a steep continuous surface
      // for an occlusion edge merely because two diagonal samples are far apart.
      var neighbors = [x > 0 ? i - 1 : i, x + 1 < columns ? i + 1 : i,
        y > 0 ? i - columns : i, y + 1 < rows ? i + columns : i]
      for (var k = 0; k < neighbors.length; k++) {
        var j = neighbors[k]
        if (depth[j] !== -100 && depth[i] - depth[j] > 0.16 && boundary[i] > 2) edge = Math.max(edge, 0.92)
      }
      if (edge > 0) { dots[i] = edge; continue }
      // No random rejection or per-part palette: each grid sample carries
      // its tone all the way to the painter, including very faint values.
      dots[i] = shading ? shading[i]
        : Math.pow(Math.max(0, Math.min(1, (0.28 - light[i]) / 0.25)), 1.8)
    }
  }
  return dots
}
