# Light renderer study

Launch from the repository root:

```sh
./tools/artwork/preview --light-study
```

Select pods, either occupied case, the empty case, or the closed case. Turn,
tilt, and scrub the pod rotation or lid opening to inspect intermediate poses.
Both columns show the same samples enlarged 2× and at actual dot size.

The left column uses the live renderer. The right starts with matching values
and exposes outlines, pod shading, light direction and frontal strength,
shadow falloff, contact shading, recess material brightness, vent darkness,
ink curve, and dot size/spacing. Scroll the controls to reach all settings;
either drag a slider or type a number. Size and spacing use half-pixel steps.
**Exterior thickness · dots** and **Internal thickness · dots** independently
control the two contour footprints: 1 is the existing
one-dot outline, 0 removes its ink, and higher values spread it across more
cells. Below one dot, fractional coverage makes the edge lighter rather than
shrinking individual dots or breaking the line. Older presets with a shared
thickness initialize both controls to that value. Outline strength independently
controls contrast. **Exterior outline contrast** adjusts the silhouette against
the background; **Internal outline contrast** adjusts creases and overlapping
parts, such as the silicone tip against the plastic body. Both start at the
current plugin values and can be set independently to 0. Overall outline strength remains a master control
for both, and existing saved presets retain their appearance. Surface shadows
and dark vent materials remain independent.
Warm paper previews a tinted foreground/background pair in both columns.

Rendering settings save automatically to `fleetpods-light-study.ini` in the
Qt generic config directory (normally `~/.config`). **Copy / load settings**
provides a JSON preset to share or restore. **Reset rendering** restores the
current plugin baseline. View controls are independent of rendering presets.

This is an isolated experiment: it does not change plugin rendering, restart
the shell, or require the daemon or physical AirPods. `Light.js` is a local
parameterized copy; `Mesh.js` uses the shared geometry and scene assembly.
When changing the production renderer, update this copy and its defaults to
keep the comparison meaningful.

Validation and screenshots:

```sh
deno run --allow-read tools/artwork/light/test.js
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  ./tools/artwork/preview --light-study --empty --capture
```

The capture writes `/tmp/fleetpods-light-study.png` using default values without
overwriting saved settings. Omit `--empty` for the pods view. Tests check default
parity across all states and multiple view angles, parameter effects and bounds,
and isolation from the reference renderer and shared geometry.
