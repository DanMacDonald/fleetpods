# Artwork conversion

`airpods-reference.blend` is the original CC0 source for the dot-mesh prototype. See [asset provenance, license, and regeneration instructions](../../plugin/artwork/README.md).

`simplify_airpods.py` runs inside Blender 4.5.3. It clears source modifiers, decimates each retained object, bakes coordinates into the renderer's convention, and exports normals and material shades. The lid keeps its source hinge; earbuds keep their docking transform. Always use `--disable-autoexec` when opening the reference file.

`compare_meshes.js` compares the generated runtime asset against an unsimplified export. The full mesh and Blender are development inputs; only `plugin/artwork/AirPodsMesh.js` is used at runtime.
