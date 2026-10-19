# AirPods dot-mesh geometry

`AirPodsMesh.js` is a simplified derivative of [Jayrenn Reeve's BlenderKit model](https://www.blendkit.com/asset-gallery-detail/27fb8b1c-fc79-41d4-9e2b-fef9d75329c1/), released under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/). The asset is titled “Airpods pro 3,” but was published in February 2023; it is used as an AirPods Pro visual reference, not as an accurate model of the later Pro 3 hardware. It is not an official Apple model.

The original mesh is retained at `tools/artwork/airpods-reference.blend` under the same CC0 dedication. Source asset ID: `27fb8b1c-fc79-41d4-9e2b-fef9d75329c1`; version ID: `7feb2bfe-1152-4d6a-98e2-67b66d5ebca0`. Retrieved October 18, 2026. The source SHA-256 is `0c5cdda97a7abff84badd4bb943fc31fff47a6ff3ba2069d20b1a358973d80e3`.

The editable artwork variant is `tools/artwork/airpods-artwork.blend`, which
the exporter uses by default. Both earbud meshes have smaller upper side vents:
52% of the reference length and 55% of its width, shifted 0.045 renderer model
units toward the silicone tips. These proportions and placement follow the
artwork study's visual references and supplied photo rather than product
measurements. The grille, inset grille,
and surrounding plastic rim move together along the shell's curved surface.
The original reference file remains untouched.

The artwork variant also levels the front lip of the lid to its center section
(0.81433 in hinge-local Z). A cross-section displacement of at most 0.00849 model
units tapers into the surrounding shell, retaining the rounded corners and
finger bevel surfaces. This removes the two shallow humps that became raised
dot rows when the lid opened at popup resolution. The edit script reproduces
both the vent and lid corrections. The rounded ends of the lid's underside
also blend the shallow outer groove into the dome, tapering the protruding
lip inward by at most 0.018 model units. This removes the small side tabs at
popup resolution while preserving the inner wall, recesses, and finger bevel.
The exposed backing strip around that skirt uses shell plastic as well;
otherwise black backing triangles can appear as missing dots along the rim.
The inner backing and pod recesses retain their original materials. The exposed
strip also shares the planar rim normal, authored in the Blender variant and
preserved through export, so the old groove cannot leave dim edge dots.

The inward-facing walls of the two lid sockets and lower charging wells use
`Artwork recess plastic`, with a shade value of 0.60 relative to the white
shell's 1.0. This material is saved in the Blender variant and exported into
the runtime mesh. Both renderers use it: dark-mode illumination is reduced,
and light mode adds material tone to the existing curved-surface shading.
The material is fully rough with zero specular response in Blender. Its
exported `matte` flag selects soft diffuse shading in the dark renderer,
without specular peaks or rim-light accents. Selection includes the rounded
inner corners of each socket so glossy shell patches cannot remain inside.
The lid selection extends farther inward to cover the complete medial walls,
while the flat central divider retains its white shell material.
The lower-well selection includes the inward bevel up to the lip plane,
preventing glossy slivers within the opening while preserving the flat rim.
The planar lips and outer shell stay white, including the pale separation
between the lid outline and its recesses. Matte material boundaries also define
the light renderer's lid recess contours: extra crease ink on the surrounding white
plastic is suppressed, preserving rounded inner corners and a pale center divider.
This cleanup is limited to the lid; the lower well retains the front case lip's
contour so no pale row cuts through its edge.

The conversion retains the case body, hinged lid, silicone tips, vents, microphone openings, stem contacts, and material shading. One earbud is mirrored at render time. Textures, scene nodes, duplicate earbud geometry, and fine tessellation are omitted. Vertices contain positions and smooth corner normals; triangles are grouped by a small set of material shades. The white case and lid batches also carry vertex-index paths for the shallow front finger bevel. These paths snap reference landmarks to existing vertices and let the light renderer trace both boundaries without changing the mesh or dark shading.

| Stored geometry | Source triangles | Simplified triangles |
| --- | ---: | ---: |
| Case body | 15,375 | 1,844 |
| Hinge | 1,428 | 214 |
| Lid | 7,603 | 1,336 |
| One earbud | 23,854 | 2,862 |
| Total | 48,260 | 6,256 |

The silicone batch includes per-vertex cavity weights derived from inward radial normals around the reference tip’s local bore axis. These preserve interior shading independently of its white material, without modifying geometry or dark-theme lighting.

The lid retains a small face patch from the reference before simplification.
Its nearly planar inner rim (within 0.005 model units of the face plane) uses a
consistent plane normal, so averaged bevel normals cannot create isolated
highlights across a flat face. The retained patch is flattened and welded at
0.02 model units to close hairline separations from the inset trim. It covers
the tiny openings through which dark backing previously appeared during
rotation. Pod recesses and curved walls retain their geometry and normals.
This is an asset correction shared by both renderers, with no image-space
blur or per-theme hole-filling rule.

The white lower corner strips share the inner rim's face normal. Their former
sideways dome normals produced dark dots that looked like chips at the enlarged
137 px popup size (71 grid cells). This changes only corner lighting normals;
the lid's vertices and triangles, recess materials, and case/pod geometry stay
the same.

The narrow divider between the lid recesses is widened by 0.012 model units
per side at its waist, tapering smoothly into the surrounding recess walls.
This keeps two popup-grid samples on the plastic lip instead of exposing the
dark backing. Both renderers use this geometry; dot size and spacing are unchanged.

The generated asset is approximately 353 KB. The complete open scene instances 9,118 triangles. Blender is only needed to regenerate assets; the plugin renders the bundled JavaScript offline.

Regenerate with Blender 4.5.3:

```sh
blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/simplify_airpods.py
```

For further manual mesh work, open `tools/artwork/airpods-artwork.blend`, edit
and save it, then run the exporter above. To rebuild that variant from the
original reference with different vent proportions, run this command first
(it overwrites the artwork variant, including any manual edits):

```sh
blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/edit_airpods.py -- \
  --length-scale 0.52 --width-scale 0.55 --tip-shift 0.045
```

Use `--output /path/to/another.blend` to save a separate variant. The edit script
starts from the original every time, so rerunning it does not compound scaling.

To compare against the full source, export it separately and run the projection comparison:

```sh
blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/simplify_airpods.py -- \
  --full --output /tmp/fleetpods-full.js
deno run --allow-read tools/artwork/compare_meshes.js /tmp/fleetpods-full.js
```

The comparison samples 192 views across four states, two grid sizes, and a full turn. It checks silhouette overlap and material-opening coverage, independently of the light-theme contour tests. Current export: 99.6% aggregate silhouette intersection-over-union, at least 98.9% in each sampled view, and 96.7% dark-feature overlap across 24 earbud views. These are sampled visual-fidelity checks against the full artwork variant, not a physical-accuracy claim.
