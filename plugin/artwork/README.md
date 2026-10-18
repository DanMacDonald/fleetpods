# AirPods dot-mesh geometry

`AirPodsMesh.js` is a simplified derivative of [Jayrenn Reeve's BlenderKit model](https://www.blendkit.com/asset-gallery-detail/27fb8b1c-fc79-41d4-9e2b-fef9d75329c1/), released under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/). The asset is titled “Airpods pro 3,” but was published in February 2023; it is used as an AirPods Pro visual reference, not as an accurate model of the later Pro 3 hardware. It is not an official Apple model.

The original mesh is retained at `tools/artwork/airpods-reference.blend` under the same CC0 dedication. Source asset ID: `27fb8b1c-fc79-41d4-9e2b-fef9d75329c1`; version ID: `7feb2bfe-1152-4d6a-98e2-67b66d5ebca0`. Retrieved October 18, 2026. The source SHA-256 is `0c5cdda97a7abff84badd4bb943fc31fff47a6ff3ba2069d20b1a358973d80e3`.

The conversion retains the case body, hinged lid, silicone tips, vents, microphone openings, stem contacts, and material shading. One earbud is mirrored at render time. Textures, scene nodes, duplicate earbud geometry, and fine tessellation are omitted. Vertices contain positions and smooth corner normals; triangles are grouped by a small set of material shades. The white case and lid batches also carry vertex-index paths for the shallow front finger bevel. These paths snap reference landmarks to existing vertices and let the light renderer trace both boundaries without changing the mesh or dark shading.

| Stored geometry | Source triangles | Simplified triangles |
| --- | ---: | ---: |
| Case body | 15,375 | 1,844 |
| Hinge | 1,428 | 214 |
| Lid | 7,603 | 1,063 |
| One earbud | 23,854 | 2,862 |
| Total | 48,260 | 5,983 |

The silicone batch includes per-vertex cavity weights derived from inward radial normals around the reference tip’s local bore axis. These preserve interior shading independently of its white material, without modifying geometry or dark-theme lighting.

The generated asset is approximately 335 KB. The complete open scene instances 8,845 triangles. Blender is only needed to regenerate assets; the plugin renders the bundled JavaScript offline.

Regenerate with Blender 4.5.3:

```sh
blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/simplify_airpods.py
```

To compare against the full source, export it separately and run the projection comparison:

```sh
blender --background --factory-startup --disable-autoexec \
  --python tools/artwork/simplify_airpods.py -- \
  --full --output /tmp/fleetpods-full.js
deno run --allow-read tools/artwork/compare_meshes.js /tmp/fleetpods-full.js
```

The comparison samples 192 views across four states, two grid sizes, and a full turn. It checks silhouette overlap and material-opening coverage, independently of the light-theme contour tests. Current export: 99.6% aggregate silhouette intersection-over-union, at least 98.9% in each sampled view, and 96.8% dark-feature overlap across 24 earbud views. These are sampled visual-fidelity checks, not a physical-accuracy claim.
