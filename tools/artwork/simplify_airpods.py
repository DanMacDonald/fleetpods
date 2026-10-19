"""Export the CC0 reference into the dot renderer's small, untextured mesh format.

blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/simplify_airpods.py
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import sys

import bpy
import bmesh
from mathutils import Matrix, Vector

ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path(__file__).with_name('airpods-artwork.blend')
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--full', action='store_true', help='Export unsimplified geometry for comparison')
parser.add_argument('--output', type=Path, help='Override output path')
args = parser.parse_args(sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else [])
if args.full and not args.output:
    parser.error('--full requires a separate --output path')
OUTPUT = args.output or ROOT / 'plugin/artwork/AirPodsMesh.js'
FULL = args.full

# Source is posed in Blender's Z-up world. Renderer uses Y-up, +Z toward camera.
AXES = Matrix(((1, 0, 0), (0, 0, 1), (0, -1, 0)))
SCALE = 20
CENTER = Vector((0, 0.30, 0))


def shade(material):
    if 'artwork_shade' in material:
        return float(material['artwork_shade'])
    if material.name == 'lifght green':
        return 1.5  # status light
    if material.name == 'metal':
        return 0.55
    if material.name == 'gold':
        return 0.4
    if material.name == 'Material.010':
        return 0.86  # silicone
    principled = next((n for n in material.node_tree.nodes if n.type == 'BSDF_PRINCIPLED'), None)
    if principled and max(principled.inputs['Base Color'].default_value[:3]) < 0.02:
        return 0.10
    return 1


def export_object(name, ratio, mode):
    obj = bpy.data.objects[name]
    bpy.context.view_layer.objects.active = obj
    # The file only uses auto-smooth node modifiers. Use its smooth vertex
    # normals directly; export never evaluates geometry nodes or source scripts.
    obj.modifiers.clear()
    obj.data.calc_loop_triangles()
    original = len(obj.data.loop_triangles)
    # Retain the reference lid's thin, nearly planar inner face. Collapsing
    # these triangles can expose its dark backing between rim/recess walls.
    # The 0.005-unit relief is below this artwork's grid resolution; its
    # shared plane normal avoids bright/dark spots from averaged rim normals.
    lid_face = []
    if mode == 'lid' and not FULL:
        face_transform = AXES * (obj.matrix_world.to_scale().x * SCALE)
        for triangle in obj.data.loop_triangles:
            if shade(obj.data.materials[triangle.material_index]) != 1:
                continue
            positions = [face_transform @ obj.data.vertices[i].co for i in triangle.vertices]
            if all(abs(p.y) < 0.005 for p in positions):
                lid_face.append([tuple(round(c, 5) for c in (*p, 0, -1, 0)) for p in positions])
        # The source also has hairline separations between this face and its
        # shallow inset trim. Close those sub-grid gaps in the retained face,
        # while leaving the much wider pod recesses and outer contour intact.
        patch = bmesh.new()
        vertices = {}
        for face in lid_face:
            corners = []
            for v in face:
                point = (v[0], 0, v[2])
                if point not in vertices:
                    vertices[point] = patch.verts.new(point)
                corners.append(vertices[point])
            if len(set(corners)) == 3:
                patch.faces.new(corners)
        bmesh.ops.remove_doubles(patch, verts=list(patch.verts), dist=0.02)
        bmesh.ops.triangulate(patch, faces=list(patch.faces))
        lid_face = [[tuple(round(c, 5) for c in (*v.co, 0, -1, 0)) for v in f.verts] for f in patch.faces]
        patch.free()
    if not FULL:
        modifier = obj.modifiers.new('Dot grid simplification', 'DECIMATE')
        modifier.ratio = ratio
        modifier.use_collapse_triangulate = True
        bpy.ops.object.modifier_apply(modifier=modifier.name)
    mesh = obj.data
    mesh.calc_loop_triangles()
    world = obj.matrix_world.copy()
    if mode == 'world':
        transform = AXES @ world.to_3x3() * SCALE
        origin = AXES @ world.translation * SCALE + CENTER
    elif mode == 'lid':
        # Bake the lid in its closed pose, around the source's actual hinge.
        transform = AXES * (world.to_scale().x * SCALE)
        origin = Vector((0, 0, 0))
    else:
        # One canonical earbud serves both sides; mirror at instance time.
        transform = Matrix.Identity(3) * (world.to_scale().x * SCALE)
        origin = Vector((0, 0, 0))
    normals = transform.inverted().transposed()
    batches = {}
    for triangle in mesh.loop_triangles:
        material = mesh.materials[triangle.material_index]
        tone = shade(material)
        batch = batches.setdefault(tone, {'shade': tone, 'vertices': [], 'triangles': [], '_indices': {}})
        if material.get('artwork_matte', False):
            batch['matte'] = True
        face = []
        planar_lid = mode == 'lid' and tone == 1 and all(
            abs((transform @ mesh.vertices[i].co + origin).y) < 0.005 for i in triangle.vertices)
        for loop_index in triangle.loops:
            loop = mesh.loops[loop_index]
            vertex = mesh.vertices[loop.vertex_index]
            position = transform @ vertex.co + origin
            normal = (normals @ mesh.corner_normals[loop_index].vector).normalized()
            if planar_lid or (mode == 'lid' and material.name == 'Artwork lid rim'):
                normal = Vector((0, -1, 0))
            data = tuple(round(c, 5) for c in (*position, *normal))
            if data not in batch['_indices']:
                batch['_indices'][data] = len(batch['vertices'])
                batch['vertices'].append(data)
            face.append(batch['_indices'][data])
        batch['triangles'].append(face)
    if lid_face:
        batch = batches[1]
        for vertices in lid_face:
            face = []
            for data in vertices:
                if data not in batch['_indices']:
                    batch['_indices'][data] = len(batch['vertices'])
                    batch['vertices'].append(data)
                face.append(batch['_indices'][data])
            batch['triangles'].append(face)
    for batch in batches.values():
        del batch['_indices']
        if mode == 'bud' and batch['shade'] == 0.86:
            # The reference tip's bore runs along local X. Its inner walls
            # share the white silicone material with the outer shell; retain
            # their inward radial normals as a separate light-theme cavity cue.
            cavity = []
            for x, y, z, nx, ny, nz in batch['vertices']:
                ry, rz = y - 0.18, z - 0.095
                radial = math.hypot(ry, rz)
                inward = -(ny * ry + nz * rz) / max(radial, 0.00001)
                cavity.append(round(max(0, min(1, (inward - 0.05) / 0.65)), 5))
            batch['cavity'] = cavity
    count = sum(len(batch['triangles']) for batch in batches.values())
    print(f'{name}: {original:,} -> {count:,} triangles')
    return list(batches.values()), original, count


bpy.ops.wm.open_mainfile(filepath=str(SOURCE))
geometry = {'body': [], 'lid': [], 'bud': []}
stats = {}
for name, ratio, key, mode in [
    ('buttom', 0.12, 'body', 'world'),
    ('Cube', 0.15, 'body', 'world'),
    ('top ', 0.14, 'lid', 'lid'),
    ('earbuds.001', 0.12, 'bud', 'bud'),
]:
    batches, before, after = export_object(name, ratio, mode)
    geometry[key].extend(batches)
    stats[name.strip()] = {'sourceTriangles': before, 'triangles': after}

lid = bpy.data.objects['top ']
geometry['hinge'] = [round(v, 5) for v in AXES @ lid.matrix_world.translation * SCALE + CENTER]
geometry['lidAngle'] = round(lid.rotation_euler.x, 6)
# Landmark the shallow finger bevel's outer boundaries on the reference mesh.
# Their normal changes are too small for reliable screen-space crease detection.
# Vertex indices keep the lines on the actual surface and moving with the lid.
for key, half_path in [
    ('body', [(0.5055, 0.3862, 0.4116), (0.5000, 0.3625, 0.4116),
              (0.4759, 0.3390, 0.4116), (0.4465, 0.3317, 0.4117)]),
    ('lid', [(0.5055, 0.3881, 0.4146), (0.5013, 0.4081, 0.4114),
             (0.4814, 0.4318, 0.4113), (0.4508, 0.4423, 0.4112)]),
]:
    batch = next(b for b in geometry[key] if b['shade'] == 1)
    offset = geometry['hinge'] if key == 'lid' else [0, 0, 0]
    path = [(-x, y, z) for x, y, z in half_path] + list(reversed(half_path))
    indices = []
    for point in path:
        target = [point[i] - offset[i] for i in range(3)]
        candidates = [i for i, v in enumerate(batch['vertices']) if v[5] > 0.95]
        index = min(candidates, key=lambda i: sum((batch['vertices'][i][j] - target[j]) ** 2 for j in range(3)))
        assert sum((batch['vertices'][index][j] - target[j]) ** 2 for j in range(3)) < 0.015 ** 2, (key, target, batch['vertices'][index])
        indices.append(index)
    batch['contours'] = [indices]
bud = bpy.data.objects['earbuds.001']
geometry['dock'] = [round(v, 5) for v in AXES @ bud.matrix_world.translation * SCALE + CENTER]
geometry['dockAngle'] = round(bud.rotation_euler.x - math.pi / 2, 6)
geometry['stats'] = stats
OUTPUT.write_text('.pragma library\n// Generated by tools/artwork/simplify_airpods.py. CC0 source: see README.md.\n'
                  'var geometry = ' + json.dumps(geometry, separators=(',', ':')) + '\n')
print(OUTPUT, OUTPUT.stat().st_size, 'bytes')
print('source sha256:', hashlib.sha256(SOURCE.read_bytes()).hexdigest())
