# Highlight art prototype

Run `./tools/artwork/preview --fresnel` from the repository root.

The three columns show an adjustable copy of the plugin's baseline lighting,
the Fresnel experiment, and **Fresnel + baseline**. **Baseline overlay** controls
how strongly brighter baseline dots fill the Fresnel image: 0% is Fresnel alone,
100% takes the brighter color at each dot. Bright Fresnel edges are preserved.
The default mix is 50%.

**Baseline frontal fill** and **Baseline face brightness** affect the left
column and its contribution to the combined image. They work for both pods and
case surfaces, excluding dark materials and matte recesses. Defaults are the approved 0.57
fill and 0.73× brightness, which match the live dark renderer.
The Fresnel column has independent settings.

The experimental renderer lives in this directory and is not imported by the
plugin. All columns use the same geometry and dot grid, with enlarged and
actual-size views.

Adjust reflection strength, edge falloff, white mix, frontal fill, and face
brightness. Switch between pods and case states, or rotate the camera to check
whether a highlight follows the shape. This is an art study, not a new approved
look; changes here are not deployed automatically.

The study starts in **Fresnel only** mode: intensity comes from
`(1 - abs(normal.z)) ** falloff`, scaled by reflection strength and material
reflectance. There is no key-light bias, specular lighting, or ambient floor.
There is no lid fill. Front-facing surfaces fade to the background; grazing
surfaces brighten. Reflection strength allows up to 4× gain in this mode.
Uncheck the mode to compare with the earlier combined
lighting experiment. Experiment fill and face-brightness controls appear only
in that mode; baseline controls remain available in both modes.
Add `--empty` to start with the empty case.

For a headless capture:

```sh
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  ./tools/artwork/preview --fresnel --capture
```

The image is saved to `/tmp/fleetpods-highlight-study.png`.
