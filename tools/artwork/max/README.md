# AirPods Max artwork study

Run `./tools/artwork/preview --max`. Rotate the headset, adjust the off-head
pose, or switch themes. The lower row shows 137 px frames beside the existing
AirPods artwork. This study runs without a daemon and uses the same Max
component as the panel.

`plugin/MaxArtwork.qml` inherits `DotPods` and supplies only a scene frame.
It shares the actual Canvas painter, physical dot grid, palette, lighting and
coverage sampling. There is no separate approximation of dot styling.
`plugin/rendering/Max.js` owns the poses. Worn cups are straightened by 27.9 degrees
around their attachment points to sit vertically along the sides of a head.
Off head, they tilt inward and slide together until their lower cushion tips
meet, without swivelling flat. The band narrows to follow their attachment
points; camera and framing stay fixed.
The panel uses the approved rear three-quarter view (yaw -2.6 radians), with
a -0.133 radian camera roll (about 7.6° clockwise) to level the cup bottoms.
The roll rotates geometry and lighting normals before rasterization, keeping
the screen's dot grid upright. It stays fixed through wear transitions.
The artwork shifts right by two dot columns to centre the strap beneath the
fixed connectors; the battery gauge and connection layout stay in place.
The closing travel is 0.15949 renderer units per cup, measured from this
simplified mesh. Band flex is an artwork approximation, not a hardware simulation.

The panel passes `headset.worn` through `SeatsView` to `AirPodsIcon`. Only
explicit `false` selects the closed pose; `true` and unknown use upright cups.
Connection ownership does not determine wear. The Max component animates
off-head over 120 ms and back on-head over 240 ms, respecting the shell's animation setting;
the study slider sets them directly.

For headless captures:

```sh
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software tools/artwork/preview --max --capture
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software tools/artwork/preview --max --light --capture
```

Images are saved as `/tmp/fleetpods-max-study.png` and
`/tmp/fleetpods-max-study-light.png`. Set `QT_SCALE_FACTOR=1.6` to check the
same study at fractional display scale.

## Asset attribution

“[Airpods max](https://sketchfab.com/3d-models/airpods-max-05181e126a6341668ca95f2d98324d30)”
by [Mr.Philin / yarik.chilin](https://sketchfab.com/yarik.chilin), licensed
under [Creative Commons Attribution 4.0](https://creativecommons.org/licenses/by/4.0/).
Downloaded November 3, 2026. This is an unofficial visual reference, not a
verified Max 2 hardware model. The author does not endorse FleetPods.

`reference.fbx` is the original FBX from the user's `airpods-max.zip`, unchanged.
The source archive's texture images are omitted; this renderer uses geometry,
normals and simple material shades. FBX SHA-256:
`25dfab5078b4958cdfea2f01372edc80580325b3faed9d82496b589395796e28`.
Archive SHA-256:
`3cdafe7e0ef775e362b09347e2933d55bbd1a24684ef818c46d3880c32ddf36b`.

FleetPods modifications: separated the 18 disconnected components into band
and cup groups, added cup attachment origins, simplified 30,569 triangles
to 7,454, normalized coordinates, and assigned untextured shell/fabric shades.
The derivative `plugin/artwork/AirPodsMaxMesh.js` and its adjacent license
retain the attribution and CC BY 4.0 terms in the installed plugin.

Regenerate with Blender 4.5.3, with automatic scripts disabled:

```sh
blender --background --factory-startup --disable-autoexec --python tools/artwork/max/export_mesh.py
```

Alternatively, with the same Blender Python module:

```sh
uv run --no-project --python 3.11 --with bpy==4.5.3 python tools/artwork/max/export_mesh.py
```
