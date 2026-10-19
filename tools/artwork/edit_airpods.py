"""Build the editable artwork variant from the untouched CC0 reference.

blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/edit_airpods.py
"""
import argparse
from collections import defaultdict
from pathlib import Path
import sys

import bpy
import numpy as np
from mathutils import Matrix, Vector

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--length-scale', type=float, default=0.52)
parser.add_argument('--width-scale', type=float, default=0.55)
parser.add_argument('--tip-shift', type=float, default=0.045,
                    help='Shift toward the silicone tip along the shell, in renderer model units')
parser.add_argument('--output', type=Path, default=ROOT / 'airpods-artwork.blend')
args = parser.parse_args(sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else [])
if not (0 < args.length_scale <= 1 and 0 < args.width_scale <= 1):
    parser.error('Vent scales must be greater than zero and no greater than one')
if not (0 <= args.tip_shift <= 0.08):
    parser.error('Tip shift must be between zero and 0.08 model units')


def material_islands(mesh, material):
    by_vertex = defaultdict(list)
    for face in mesh.polygons:
        if mesh.materials[face.material_index].name == material:
            for i in face.vertices:
                by_vertex[i].append(face.index)
    remaining = {i for faces in by_vertex.values() for i in faces}
    result = []
    while remaining:
        pending, vertices = [min(remaining)], set()
        while pending:
            i = pending.pop()
            if i not in remaining:
                continue
            remaining.remove(i)
            for j in mesh.polygons[i].vertices:
                vertices.add(j)
                pending.extend(k for k in by_vertex[j] if k in remaining)
        result.append(sorted(vertices))
    return result


def smooth_falloff(distance, inner, outer):
    t = np.clip((distance - inner) / (outer - inner), 0, 1)
    return 1 - t * t * (3 - 2 * t)


def edit_vent(obj):
    mesh = obj.data
    unit = abs(obj.matrix_world.to_scale().x) * 20
    points = np.array([tuple(v.co) for v in mesh.vertices]) * unit
    # The upper side vent has an outer grille and a second inset grille.
    # Select by material and location, not mutable face/vertex indices.
    islands = [ids for ids in material_islands(mesh, 'Material.003')
               if points[ids].mean(axis=0)[1] > 0.2 and points[ids].mean(axis=0)[2] > 0.3]
    assert len(islands) == 2, 'Reference side-vent topology changed'
    target = sorted(set(i for ids in islands for i in ids))
    outer = points[max(islands, key=len)]
    center = outer.mean(axis=0)
    _, axes = np.linalg.eigh(np.cov((outer - center).T))
    # Local coordinates: long tangent, short tangent, outward normal.
    frame = axes[:, [2, 1, 0]]
    if frame[2, 2] < 0:
        frame[:, 2] *= -1
    bounds = (outer - center) @ frame
    center += frame[:, :2] @ ((bounds[:, :2].min(axis=0) + bounds[:, :2].max(axis=0)) / 2)
    local = (points - center) @ frame
    radii = np.ptp(bounds[:, :2], axis=0) / 2
    radius = np.linalg.norm(local[:, :2] / radii, axis=1)
    weight = smooth_falloff(radius, 1.15, 2.1) * smooth_falloff(abs(local[:, 2]), 0.045, 0.12)

    plastic, protected = set(), set()
    for face in mesh.polygons:
        if mesh.materials[face.material_index].name == 'Material.002':
            plastic.update(face.vertices)
        else:
            protected.update(face.vertices)
    protected.difference_update(target)
    allowed = plastic | set(target)
    weight[[i for i in range(len(points)) if i not in allowed or i in protected]] = 0
    weight[target] = 1

    # Keep the smaller opening on the curved shell rather than pulling its
    # rim into a flat decal. Fit the surrounding plastic and retain each
    # vertex's depth relative to that surface as its tangential position moves.
    normals = np.array([tuple(v.normal) for v in mesh.vertices])
    fit = np.array([i for i in plastic if 0.9 < radius[i] < 2.1
                    and abs(local[i, 2]) < 0.08 and normals[i] @ frame[:, 2] > 0.5])
    assert len(fit) >= 12, 'Not enough surrounding shell samples'

    def basis(uv):
        u, v = uv[:, 0], uv[:, 1]
        return np.column_stack((u * u, u * v, v * v, u, v, np.ones(len(u))))

    curve = np.linalg.lstsq(basis(local[fit]), local[fit, 2], rcond=None)[0]
    moved = local.copy()
    moved[:, 0] *= 1 - weight * (1 - args.length_scale)
    moved[:, 1] *= 1 - weight * (1 - args.width_scale)
    # The tip is on local -X. Move the complete opening and its surrounding
    # rim tangentially, then follow the shell's curvature below. This also
    # lets the vent foreshorten naturally as the pod turns.
    shift = np.array([-args.tip_shift, 0, 0]) @ frame
    moved[:, :2] += weight[:, None] * shift[:2]
    moved[:, 2] += (basis(moved) - basis(local)) @ curve
    result = (center + moved @ frame.T) / unit
    for i in np.flatnonzero(weight):
        mesh.vertices[int(i)].co = result[i]
    mesh.update()
    obj['artwork_vent_length_scale'] = args.length_scale
    obj['artwork_vent_width_scale'] = args.width_scale
    obj['artwork_vent_tip_shift'] = args.tip_shift
    print(f'{obj.name}: edited {len(target)} vent vertices; adjusted {np.count_nonzero(weight)} shell/vent vertices')


def smooth_lid_divider(obj):
    # The inner lip narrows just enough at its waist to expose two backing
    # samples on the popup grid. Widen that section by 0.012 units per side,
    # tapering through the curved recess walls so the lip stays continuous.
    unit = abs(obj.matrix_world.to_scale().x) * 20
    points = np.array([tuple(v.co) for v in obj.data.vertices]) * unit
    weight = smooth_falloff(abs(points[:, 0]), 0.12, 0.25)
    weight *= smooth_falloff(abs(-points[:, 1] - 0.445), 0.045, 0.18)
    weight *= smooth_falloff(abs(points[:, 2]), 0.01, 0.12)
    displacement = np.sign(points[:, 0]) * 0.012 * weight
    for i in np.flatnonzero(abs(displacement) > 1e-8):
        obj.data.vertices[int(i)].co.x += displacement[i] / unit
    obj.data.update()
    obj['artwork_lid_divider_widening'] = 0.012
    print(f'{obj.name}: smoothed {np.count_nonzero(displacement)} inner divider vertices')


def smooth_lid_recess_sides(obj):
    # Carry the outer socket walls farther up their curved mouths. At popup
    # density this joins the two low dark samples to the recess above, rather
    # than leaving a short isolated notch beside a seated pod.
    unit = abs(obj.matrix_world.to_scale().x) * 20
    points = np.array([tuple(v.co) for v in obj.data.vertices]) * unit
    weight = smooth_falloff(abs(abs(points[:, 0]) - 1.0), 0.04, 0.13)
    weight *= smooth_falloff(abs(-points[:, 1] - 0.43), 0.055, 0.19)
    weight *= smooth_falloff(abs(points[:, 2]), 0.01, 0.16)
    displacement = np.sign(points[:, 0]) * 0.035 * weight
    for i in np.flatnonzero(abs(displacement) > 1e-8):
        obj.data.vertices[int(i)].co.x += displacement[i] / unit
    obj.data.update()
    obj['artwork_lid_recess_side_extension'] = 0.035
    print(f'{obj.name}: smoothed {np.count_nonzero(displacement)} outer recess vertices')


def smooth_lid_skirt(obj):
    # Blend the shallow groove and underside lip into the dome. Its outer wall
    # steps inward and back out before joining the dome; at dot-grid scale
    # that makes the lip read as two tabs. Fit a smooth section and taper the
    # lip inward by 0.018 units, preserving the inner wall, recesses and central
    # finger bevel. Work on the rounded ends, blending into the straight rim.
    unit = abs(obj.matrix_world.to_scale().x) * 20
    points = np.array([tuple(v.co) for v in obj.data.vertices]) * unit
    p = points[:, [0, 2, 1]].copy()
    p[:, 2] *= -1
    dx = np.maximum(abs(p[:, 0]) - 0.734, 0)
    dz = p[:, 2] - 0.411
    radius = np.hypot(dx, dz)
    angle = np.arctan2(dz, dx)
    rim = (abs(p[:, 1]) < 0.00001) & (radius > 0.39) & (abs(p[:, 0]) > 0.74)
    order = np.argsort(angle[rim])
    base = np.interp(angle, angle[rim][order], radius[rim][order])
    target = base * np.sqrt(np.maximum(0, 1 - (p[:, 1] / 0.50) ** 2))
    target -= 0.018 * smooth_falloff(p[:, 1], 0, 0.14)
    weight = 1 - smooth_falloff(radius / base, 0.95, 0.97)
    weight *= smooth_falloff(p[:, 1], 0.08, 0.14)
    weight *= 1 - smooth_falloff(abs(p[:, 0]), 0.70, 0.90)
    weight[p[:, 1] < -0.00001] = 0
    displacement = (target - radius) * weight
    p[:, 0] += np.sign(p[:, 0]) * dx / np.maximum(radius, 1e-8) * displacement
    p[:, 2] += dz / np.maximum(radius, 1e-8) * displacement
    for i in np.flatnonzero(abs(displacement) > 1e-8):
        obj.data.vertices[int(i)].co = (p[i, 0] / unit, -p[i, 2] / unit, p[i, 1] / unit)
    obj.data.update()
    obj['artwork_smooth_lid_skirt'] = True
    print(f'{obj.name}: smoothed {np.count_nonzero(abs(displacement) > 1e-8)} skirt vertices; max shift {max(abs(displacement)):.5f}')
    # The groove's black backing wraps around the outer skirt. After leveling
    # the lip it can still appear between white rim triangles. Treat that
    # exposed strip as continuous shell plastic, leaving the inner backing
    # and the two pod recesses alone.
    plastic = next(i for i, m in enumerate(obj.data.materials) if m.name == 'Material.002')
    rim_material = obj.data.materials[plastic].copy()
    rim_material.name = 'Artwork lid rim'
    obj.data.materials.append(rim_material)
    rim_index = len(obj.data.materials) - 1
    recolored = 0
    for face in obj.data.polygons:
        ids = list(face.vertices)
        material = obj.data.materials[face.material_index].name
        # The white lower corner strip also retains the dome's sideways
        # normals. At a 71-cell popup grid those faces become isolated dark
        # dots in the lip. Give this narrow outer strip the same face normal
        # as the adjacent rim; the recessed walls lie farther inward in Z.
        lower_corner = (material == 'Material.002' and min(abs(p[ids, 0])) > 0.70
                        and max(p[ids, 2]) < 0.15 and min(p[ids, 1]) > -0.006
                        and max(p[ids, 1]) < 0.18)
        if material != 'Material.001' and not lower_corner:
            continue
        if (lower_corner
                or min(radius[ids]) > 0.365 and min(p[ids, 1]) < 0.07 and max(p[ids, 1]) < 0.15
                or min(abs(p[ids, 0])) > 0.78 and min(radius[ids]) > 0.29 and max(p[ids, 1]) < 0.30):
            face.material_index = rim_index
            recolored += 1
    print(f'{obj.name}: matched {recolored} outer backing faces to the shell material')
    # This strip completes the planar inside lip. Its former inward dome
    # normals made isolated dim dots even after the material was corrected.
    # Author the face normal in Blender too; the exporter preserves this
    # surface designation through decimation rather than averaging it away.
    normals = [tuple(n.vector) for n in obj.data.corner_normals]
    for face in obj.data.polygons:
        if face.material_index == rim_index:
            for loop in face.loop_indices:
                normals[loop] = (0, 0, -1)
    obj.data.normals_split_custom_set(normals)


def darken_recesses(obj, lid):
    # Select the inward-facing white walls below each mouth, in the same
    # coordinate system as the renderer. Outer plastic and the planar lip
    # remain white; vents, contacts and backing retain their own materials.
    axes = Matrix(((1, 0, 0), (0, 0, 1), (0, -1, 0)))
    transform = axes * (obj.matrix_world.to_scale().x * 20) if lid else axes @ obj.matrix_world.to_3x3() * 20
    origin = Vector((0, 0, 0)) if lid else axes @ obj.matrix_world.translation * 20 + Vector((0, 0.30, 0))
    normals = transform.inverted().transposed()
    plastic = next(m for m in obj.data.materials if m.name == 'Material.002')
    material = bpy.data.materials.get('Artwork recess plastic')
    if material is None:
        material = plastic.copy()
        material.name = 'Artwork recess plastic'
        material['artwork_shade'] = 0.60
        material['artwork_matte'] = True
        material.diffuse_color = (0.60, 0.60, 0.60, 1)
        node = next(n for n in material.node_tree.nodes if n.type == 'BSDF_PRINCIPLED')
        node.inputs['Base Color'].default_value = (0.60, 0.60, 0.60, 1)
        node.inputs['Roughness'].default_value = 1
        node.inputs['Specular IOR Level'].default_value = 0
    obj.data.materials.append(material)
    index = len(obj.data.materials) - 1
    count = 0
    for face in obj.data.polygons:
        if obj.data.materials[face.material_index].name != 'Material.002':
            continue
        p = transform @ face.center + origin
        n = (normals @ face.normal).normalized()
        center = Vector((0.60 if p.x >= 0 else -0.60, 0 if lid else 0.386, 0.411 if lid else 0))
        # The sockets are rounded rectangles, not ellipses. Include their
        # inner corner walls so they cannot retain glossy white patches.
        # Lid cavities extend farther toward the central divider than toward
        # the outside rim. Include the complete medial wall; its lowest-X
        # strip is still recessed even though it lies beyond the outer radius.
        width = 0.60 if lid and abs(p.x) < 0.60 else 0.46
        mouth = ((p.x - center.x) / width) ** 4 + ((p.z - center.z) / 0.36) ** 4
        # The lower well's inward bevel reaches almost to the 0.386 lip
        # plane. Stopping at 0.37 leaves glossy slivers inside the opening.
        # The inward-normal test excludes the flat rim and exterior shell.
        below_lip = p.y > 0.008 if lid else p.y < 0.385
        if below_lip and mouth < 1 and n.dot(center - p) > 0.01:
            face.material_index = index
            count += 1
    assert count > 100, 'Recess selection no longer matches the reference'
    print(f'{obj.name}: darkened {count} recess faces')


def straighten_lid_rim(obj):
    # The front lip bows forward on either side of the finger bevel. When
    # opened, that sub-grid relief becomes two raised silhouette steps. Level
    # the lip to the center section, tapering the displacement into the shell.
    # Translate each cross-section together to retain its thickness and the
    # bevel's upper/lower surfaces; rounded corners below this plane stay put.
    unit = abs(obj.matrix_world.to_scale().x) * 20
    points = np.array([tuple(v.co) for v in obj.data.vertices]) * unit
    canonical = points[:, [0, 2, 1]].copy()
    canonical[:, 2] *= -1
    rim = canonical[(abs(canonical[:, 1]) < 0.005) & (canonical[:, 2] > 0.79)]
    profile = {}
    for x, y, z in rim:
        key = round(abs(x), 4)
        profile[key] = max(profile.get(key, 0), z)
    xs = sorted(profile)
    relief = np.maximum(0, np.interp(abs(canonical[:, 0]), xs,
                                    [profile[x] for x in xs]) - 0.81433)
    weight = smooth_falloff(abs(canonical[:, 1]), 0.005, 0.08)
    weight *= 1 - smooth_falloff(canonical[:, 2], 0.74, 0.80)
    displacement = relief * weight
    # Canonical +Z is Blender-local -Y.
    for i in np.flatnonzero(displacement > 1e-8):
        obj.data.vertices[int(i)].co.y += displacement[i] / unit
    obj.data.update()
    obj['artwork_lid_rim_plane'] = 0.81433
    print(f'{obj.name}: leveled {np.count_nonzero(displacement > 1e-8)} rim vertices; max shift {max(displacement):.5f}')


bpy.ops.wm.open_mainfile(filepath=str(ROOT / 'airpods-reference.blend'))
bpy.context.view_layer.update()
for name in ('earbuds.001', 'earbuds.002'):
    edit_vent(bpy.data.objects[name])
straighten_lid_rim(bpy.data.objects['top '])
smooth_lid_divider(bpy.data.objects['top '])
smooth_lid_recess_sides(bpy.data.objects['top '])
smooth_lid_skirt(bpy.data.objects['top '])
darken_recesses(bpy.data.objects['top '], True)
darken_recesses(bpy.data.objects['buttom'], False)
bpy.context.scene['artwork_source'] = 'airpods-reference.blend (CC0); smaller side vents toward tips, level lid lip, smooth outer skirt and darker recessed plastic for the dot-mesh study'
bpy.context.preferences.filepaths.save_version = 0
bpy.ops.wm.save_as_mainfile(filepath=str(args.output))
