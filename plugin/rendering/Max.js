.import "../DotMesh.js" as Mesh

// Worn cups sit vertically beside the head. Off head, they tilt inward and
// their lower cushion tips meet as the band narrows. No flat swivel is used.
function frame(geometry, rest, yaw, pitch, columns, rows, ink, roll) {
  var closed = Math.max(0, Math.min(1, rest))
  // Distance from the centreline to the simplified cushion's innermost tip.
  var travel = closed * 0.15949
  // The source cushions lean inward by 27.9 degrees in the frontal plane.
  // Undo that lean when worn; retain their source tilt when resting closed.
  var wornTilt = (1 - closed) * 27.9 * Math.PI / 180
  var bandWidth = 1 - travel / geometry.pivots.right[0]
  var scene = []
  function add(name, side, id) {
    var pivot = geometry.pivots[name]
    for (var i = 0; i < geometry[name].length; i++) {
      var source = geometry[name][i], mesh = source
      if (name === 'band' && travel > 0) {
        var vertices = []
        for (var j = 0; j < source.vertices.length; j++) {
          var v = source.vertices[j]
          // Inverse-transpose normal for the band's horizontal compression.
          var nx = v[3] / bandWidth
          var length = Math.hypot(nx, v[4], v[5])
          vertices.push([v[0] * bandWidth, v[1], v[2], nx / length, v[4] / length, v[5] / length])
        }
        mesh = {vertices: vertices, triangles: source.triangles, matte: source.matte}
      }
      scene.push({mesh: mesh, at: [pivot[0] - side * travel, pivot[1], pivot[2]],
        angle: [0, 0, side * wornTilt], mirror: 1, scale: 1, shade: source.shade, surfaceId: id})
    }
  }
  add('band', 0, 1)
  add('left', -1, 2)
  add('right', 1, 3)
  return Mesh.renderScene(scene, {state: '', lid: 1, yaw: yaw, pitch: pitch, roll: roll,
    columns: columns, rows: rows, scale: Math.min(columns, rows) / 3.0}, ink, true)
}
