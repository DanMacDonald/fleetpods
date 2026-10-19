.import "Raster.js" as Raster

// Light-background renderer: matte volumes, geometric contours, and pale
// docked shells. All light-specific policy lives here; the rasterizer only
// supplies projected surface samples. render(scene, view) is the entry point.
function render(scene, view) {
  var columns = view.columns, rows = view.rows, scale = view.scale
  var state = view.state, lid = view.lid
  var separatePods = state !== "" && lid > 0.03
  var dockedPods = {}, shadingGroups = [], openingSurfaces = {}
  for (var p = 0; p < scene.length; p++) {
    var part = scene[p]
    if (part.dockedPod) dockedPods[part.surfaceId] = true
    if (part.dockedPod || state === "") openingSurfaces[part.surfaceId] = true
    if (separatePods) shadingGroups[part.surfaceId] = part.dockedPod ? part.surfaceId : 1
  }
  // Keep complete case and pod layers so hidden recess samples still support
  // shading. Only the final depth test determines which layer is visible.
  var samples = Raster.rasterize(scene, view, {
    vertexTone: vertexTone, surfaceData: true, groups: shadingGroups,
    openingSurfaces: openingSurfaces
  })
  var depth = samples.depth, light = samples.light, facing = samples.facing
  var normals = samples.normals, surfaces = samples.surfaces
  var materials = samples.materials, cavities = samples.cavities
  var contours = samples.contours, layers = samples.layers
  var edges = featureEdges(depth, normals, surfaces, columns, rows, state === "")
  if (state !== "" && lid <= 0.03) closedCaseSeam(edges, depth, surfaces, columns, rows)
  var traced = meshContours(contours, depth, surfaces, columns, rows)
  // Inside the finger bevel, the part join is a soft contact shadow. Its two
  // outer boundaries carry the outline instead of a third solid center line.
  for (var i = 0; i < edges.length; i++) edges[i] = Math.max(traced.interior[i] ? 0 : edges[i], traced.edges[i])
  var shading = separatePods ? composeSurfaceShading(layers, shadingGroups, surfaces, columns, rows, scale)
    : surfaceShading(light, depth, normals, materials, columns, rows, scale)
  var values = stipple(light, depth, facing, columns, rows, edges, shading)
  lidMaterialBoundaries(values, shading, samples, scene, columns, rows)
  dockedPodContrast(values, shading, depth, surfaces, materials, dockedPods, columns, rows, edges, normals, cavities)
  lidRimSeparation(values, samples, scene, view)
  for (var i = 0; i < values.length; i++) values[i] = Math.max(values[i], samples.openingCoverage[i])
  dockedOpeningSeparation(values, samples, dockedPods, columns, rows)
  return { values: values, tonal: true }
}

// A dark matte lid socket already defines its edge against white plastic. Crease
// detection can put an additional ink row on the white side, making rounded
// sockets wider and leaving alternating knots across a narrow divider. Keep
// the actual material boundary and the white surface's own volume shading.
// This applies to the lid face only: the lower well meets the front shell at
// a real lip, whose contour must remain visible beside the dark well material.
function lidMaterialBoundaries(values, shading, samples, scene, columns, rows) {
  var matte = {}
  for (var p = 0; p < scene.length; p++) {
    var part = scene[p]
    if (part.surfaceId !== 2 || !part.mesh.matte || part.shade >= 0.95) continue
    if (!matte[part.surfaceId]) matte[part.surfaceId] = []
    matte[part.surfaceId].push(part.shade)
  }
  var depth = samples.depth, surfaces = samples.surfaces, materials = samples.materials
  var boundary = silhouetteDistance(depth, columns, rows)
  for (var y = 1; y < rows - 1; y++) for (var x = 1; x < columns - 1; x++) {
    var i = y * columns + x, shades = matte[surfaces[i]]
    if (!shades || materials[i] < 0.95 || boundary[i] <= 1 || values[i] <= shading[i]) continue
    var contrastEdge = false
    for (var dy = -1; dy <= 1 && !contrastEdge; dy++) for (var dx = -1; dx <= 1; dx++) {
      var j = i + dy * columns + dx
      if (surfaces[j] !== surfaces[i] || depth[j] > depth[i] + 0.01) continue
      for (var k = 0; k < shades.length; k++) {
        if (Math.abs(materials[j] - shades[k]) < 0.0001) { contrastEdge = true; break }
      }
    }
    if (contrastEdge) values[i] = shading[i]
  }
}

// Keep white shell between a small vent and the lid behind it. Limit this to
// the visible pod boundary near an opening; never lighten the lid or draw a
// halo outside the pod, and keep the opening's core/soft footprint intact.
function dockedOpeningSeparation(values, samples, dockedPods, columns, rows) {
  var surfaces = samples.surfaces, materials = samples.materials, depth = samples.depth
  var coverage = samples.openingCoverage
  for (var y = 1; y < rows - 1; y++) for (var x = 1; x < columns - 1; x++) {
    var i = y * columns + x, surface = surfaces[i]
    if (!dockedPods[surface] || materials[i] < 0.95 || coverage[i] > 0.2) continue
    if (depth[i - 1] === -100 || depth[i + 1] === -100
        || depth[i - columns] === -100 || depth[i + columns] === -100) continue
    var lidBehind = false
    for (var dy = -1; dy <= 1; dy++) for (var dx = -1; dx <= 1; dx++) {
      var j = (y + dy) * columns + x + dx
      if (surfaces[j] === 2 && depth[j] < depth[i]) lidBehind = true
    }
    if (!lidBehind) continue
    var nearOpening = false
    for (var dy = -2; dy <= 2; dy++) for (var dx = -2; dx <= 2; dx++) {
      var sx = x + dx, sy = y + dy
      if (sx < 0 || sy < 0 || sx >= columns || sy >= rows) continue
      var j = sy * columns + sx
      if (surfaces[j] === surface && materials[j] <= 0.2) nearOpening = true
    }
    if (nearOpening) values[i] = Math.min(values[i], 0.02)
  }
}

// At small grids the white lip can share a row with the dark socket wall.
// Reserve one pale inner sample beside the exposed lid lip, including diagonal
// steps. A socket can occupy that sample on coarse grids; the pale row takes
// precedence so its shadow cannot merge with the silhouette.
function lidRimSeparation(values, samples, scene, view) {
  if (view.state === "" || view.lid < 0.3) return
  var lidPart = null
  for (var p = 0; p < scene.length; p++) {
    if (scene[p].surfaceId === 2) { lidPart = scene[p]; break }
  }
  if (!lidPart) return
  var a = lidPart.angle
  var face = Raster.rotate(0, -1, 0, a[0], a[1], a[2])
  face = Raster.rotate(face[0], face[1], face[2], view.pitch, view.yaw, 0)
  if (face[2] < 0.5) return
  var depth = samples.depth
  var surfaces = samples.surfaces, materials = samples.materials
  var columns = view.columns, rows = view.rows
  var boundary = silhouetteDistance(depth, columns, rows)
  for (var y = 1; y < rows - 1; y++) for (var x = 1; x < columns - 1; x++) {
    var i = y * columns + x
    if (surfaces[i] !== 2 || boundary[i] <= 1) continue
    var neighbors = [i - 1, i + 1, i - columns, i + columns,
      i - columns - 1, i - columns + 1, i + columns - 1, i + columns + 1]
    for (var k = 0; k < neighbors.length; k++) {
      var j = neighbors[k]
      if (surfaces[j] !== 2 || materials[j] < 0.95 || boundary[j] !== 1) continue
      values[i] = Math.min(values[i], 0.02)
      break
    }
  }
}

// Retained for surface samples; the final tone comes from interpolated
// normals and neighborhood geometry, never specular or rim illumination.
function vertexTone(n, shade) {
  var diffuse = Math.max(0, -0.45 * n[0] + 0.65 * n[1] + 0.61 * n[2])
  return Math.min(1, (0.20 + 0.80 * diffuse) * shade)
}

// Continuous ink fades into the actual background, on a full dot lattice.
function dotStyle(pitch, background, shadow) {
  return { levels: 64, size: Math.max(1.05, pitch * 0.48), base: background,
    exponent: 0.75, level: function(value) { return Math.round(value * 64) } }
}

// The closed lid/body join is one shallow, mostly horizontal boundary. Trace
// its lid side consistently instead of alternating sides by sub-cell depth.
// Nearby normal creases describe the same join and must not form extra knots.
// Explicit finger-bevel contours are applied after this pass and remain intact.
function closedCaseSeam(edges, depth, surfaces, columns, rows) {
  var seam = new Int32Array(columns)
  for (var x = 0; x < columns; x++) {
    seam[x] = -1
    for (var y = 0; y < rows - 1; y++) {
      var i = y * columns + x
      if (surfaces[i] === 2 && surfaces[i + columns] === 1
          && (seam[x] < 0 || depth[i] > depth[seam[x] * columns + x])) seam[x] = y
    }
  }
  for (var x = 0; x < columns; x++) {
    if (seam[x] < 0) continue
    for (var dy = -2; dy <= 2; dy++) for (var dx = -1; dx <= 1; dx++) {
      var sx = x + dx, sy = seam[x] + dy
      if (sx >= 0 && sx < columns && sy >= 0 && sy < rows) edges[sy * columns + sx] = 0
    }
  }
  for (var x = 0; x < columns; x++) {
    if (seam[x] >= 0) edges[seam[x] * columns + x] = 0.94
  }
}

// Shade complete layers first, then select the visible surface's tone. Masking
// before the neighborhood passes removes cavity samples near overlaps and
// creates a light halo. Hidden samples can support shading, but never draw.
function composeSurfaceShading(layers, groups, surfaces, columns, rows, scale) {
  var result = new Float32Array(surfaces.length)
  for (var group = 0; group < layers.length; group++) {
    var layer = layers[group]
    if (!layer) continue
    var tone = surfaceShading(null, layer.depth, layer.normals, layer.materials, columns, rows, scale)
    for (var i = 0; i < result.length; i++) {
      if (groups[surfaces[i]] === group) result[i] = tone[i]
    }
  }
  return result
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
    var exposed = false, joint = false, overlapBoundary = false, hardBend = false
    for (var k = 0; k < neighbors.length; k++) {
      var j = neighbors[k]
      if (depth[j] === -100) exposed = true
      // Silicone-to-shell joins are actual internal boundaries, including
      // where a tip turns edge-on. Trace just the lower-shade material side.
      if (surfaces[j] === surfaces[i] && materials[j] > materials[i] + 0.04) joint = true
      if (depth[j] !== -100 && surfaces[j] !== surfaces[i] && depth[j] < depth[i]) overlapBoundary = true
      if (normals && surfaces[j] === surfaces[i]) {
        var bend = 1 - normals[i * 3] * normals[j * 3]
          - normals[i * 3 + 1] * normals[j * 3 + 1] - normals[i * 3 + 2] * normals[j * 3 + 2]
        if (bend > 0.15 || Math.abs(depth[i] - depth[j]) > 0.08) hardBend = true
      }
    }
    if (exposed) continue
    // Keep the full volume gradient in a lighter tonal range than the case
    // recesses. The previous near-white range lost most of its intermediate
    // dots to palette quantization, leaving only the dark feature traces.
    var pale = Math.pow(shading[i], 0.85) * 0.34
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
    // The general crease detector also responds to smooth shell curvature.
    // Only genuine sharp bends or depth breaks should become dark pod details.
    var detail = joint ? 0.8 : overlapBoundary ? 0.7 : internal && hardBend && features ? features[i] * 0.85 : 0
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
function featureEdges(depth, normals, surfaces, columns, rows, sharpOnly) {
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
  function sharp(a, b) {
    if (depth[a] === -100 || depth[b] === -100 || surfaces[a] !== surfaces[b]) return false
    return bend(a, b) > 0.15 || Math.abs(depth[a] - depth[b]) > 0.08
  }
  for (var y = 1; y < rows - 1; y++) {
    for (var x = 1; x < columns - 1; x++) {
      var i = y * columns + x
      if (depth[i] === -100) continue
      horizontal[i] = bend(i - 1, i + 1)
      vertical[i] = bend(i - columns, i + columns)
      // A smooth stem can exceed the broad two-cell bend threshold as its
      // curvature moves across the grid. Require a local sharp bend or depth
      // break before promoting standalone pod shading to a crease line.
      if (sharpOnly) {
        if (!sharp(i - 1, i) && !sharp(i, i + 1)) horizontal[i] = 0
        if (!sharp(i - columns, i) && !sharp(i, i + columns)) vertical[i] = 0
      }
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
      var volume = Math.min(0.9, tone + (ambient * 0.65 + relief[i]) * (1 - tone))
      // Material reflectance is shared with the dark renderer. Gray plastic
      // adds ink while retaining its diffuse/occlusion gradients; white
      // plastic keeps its existing volume shading.
      var absorption = Math.max(0, Math.min(1, (1 - materials[i]) / 0.8))
      density[i] = volume + absorption * (1 - volume)
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
