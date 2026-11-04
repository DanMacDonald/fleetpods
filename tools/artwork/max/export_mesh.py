"""Export Mr.Philin's CC BY 4.0 Max mesh for the panel and QML study.

blender --background --factory-startup --disable-autoexec --python tools/artwork/max/export_mesh.py
"""
import hashlib
import json
from pathlib import Path

import bpy
from mathutils import Matrix, Vector

HERE = Path(__file__).resolve().parent
SOURCE = HERE / 'reference.fbx'
AXES = Matrix(((1, 0, 0), (0, 0, 1), (0, -1, 0)))
CENTER = Vector((0, 0, 0.8114))

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.context.preferences.filepaths.use_scripts_auto_execute = False
bpy.ops.import_scene.fbx(filepath=str(SOURCE))
obj = bpy.context.selected_objects[0]
bpy.context.view_layer.objects.active = obj
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_all(action='SELECT')
bpy.ops.mesh.separate(type='LOOSE')
bpy.ops.object.mode_set(mode='OBJECT')

geometry = {'band': [], 'left': [], 'right': [], 'pivots': {}}
source_triangles = output_triangles = 0
for obj in sorted(bpy.context.selected_objects, key=lambda o: o.name):
    mesh = obj.data
    points = [obj.matrix_world @ v.co for v in mesh.vertices]
    low = Vector(tuple(min(p[i] for p in points) for i in range(3)))
    high = Vector(tuple(max(p[i] for p in points) for i in range(3)))
    side = 1 if (low.x + high.x) > 0 else -1
    part = 'band' if high.z > 0.87 else ('right' if side > 0 else 'left')
    # Source has no pivots. These stem attachment origins are study inputs.
    pivot = Vector((side * 0.515, 0, 0.82)) if part != 'band' else CENTER
    geometry['pivots'][part] = [round(v, 6) for v in AXES @ (pivot - CENTER) * 2]
    material = mesh.materials[mesh.polygons[0].material_index].name
    cushion = material in ('Pod1', 'Pod2') and high.z > 0.8
    matte = cushion or 'cloth' in material.lower()
    mesh.calc_loop_triangles()
    before = len(mesh.loop_triangles)
    source_triangles += before
    if before > 120:
        bpy.context.view_layer.objects.active = obj
        modifier = obj.modifiers.new('Dot study simplification', 'DECIMATE')
        modifier.ratio = 0.24
        modifier.use_collapse_triangulate = True
        bpy.ops.object.modifier_apply(modifier=modifier.name)
    mesh = obj.data
    mesh.calc_loop_triangles()
    output_triangles += len(mesh.loop_triangles)
    batch = {'vertices': [], 'triangles': [], 'shade': 0.70 if matte else 1, 'matte': matte}
    indices = {}
    normals = obj.matrix_world.to_3x3().inverted().transposed()
    for triangle in mesh.loop_triangles:
        corners = []
        for vertex, loop in zip(triangle.vertices, triangle.loops):
            p = AXES @ (obj.matrix_world @ mesh.vertices[vertex].co - pivot) * 2
            n = (AXES @ normals @ mesh.corner_normals[loop].vector).normalized()
            key = tuple(round(v, 5) for v in (*p, *n))
            if key not in indices:
                indices[key] = len(batch['vertices'])
                batch['vertices'].append(key)
            corners.append(indices[key])
        batch['triangles'].append(corners)
    geometry[part].append(batch)

geometry['sourceTriangles'] = source_triangles
geometry['triangles'] = output_triangles
header = '// Airpods max by Mr.Philin (yarik.chilin), CC BY 4.0. See AirPodsMaxMesh.LICENSE.md.\n'
header += '// Simplified, regrouped and normalized by FleetPods; textures omitted.\n'
header += '// Source FBX SHA-256: ' + hashlib.sha256(SOURCE.read_bytes()).hexdigest() + '\n'
(HERE.parents[2] / 'plugin/artwork/AirPodsMaxMesh.js').write_text('.pragma library\n' + header + 'var geometry = ' + json.dumps(geometry, separators=(',', ':')) + '\n')
print(f'Max study: {source_triangles:,} → {output_triangles:,} triangles')
