# Dot mesh artwork study

For isolated highlight experiments, run `./tools/artwork/preview --fresnel`.
This [separate art prototype](../../tools/artwork/fresnel/README.md) compares the
plugin baseline with adjustable experimental lighting at enlarged and actual
dot sizes. Its experimental renderer is not used by the live plugin.

Run the interactive preview from the repository root:

```sh
./tools/artwork/preview --light --open
```

Drag the large artwork or use the rotation slider. Switch between earbuds, an open case, a closed case, either pod out, or both out; pause the motion, adjust the grid spacing, or switch the background. Use Open and Left out / Right out / Both out to review the lift/return tweens, or launch with `--left-out`, `--right-out`, or `--empty`. The bottom row shows the artwork at the popup's 128-pixel size.

Use `./tools/artwork/preview --pods-out` for the standalone pair's entrance,
beside the original 2D illustration. Replay or scrub the descent and vertical-axis
turn, adjust dot spacing, and switch between light and dark. It uses the popup's `BudsMesh.qml`.
`--capture` saves `/tmp/fleetpods-pods-out.png` (or `-light.png` with `--light`).

This study uses a simplified CC0 AirPods Pro reference mesh, with separate case and lid geometry and mirrored earbuds. See [asset provenance and regeneration](../artwork/README.md). `DotMesh.js` assembles the scene and selects one renderer for the background. `DotPods.qml` draws the resulting grid with square dots. It runs at 20 frames per second while visible; hiding or pausing it stops the timer. Colors are component inputs.

The rendering modules have distinct responsibilities:

- [`rendering/Light.js`](../rendering/Light.js) owns all light-theme treatment: matte volume gradients, recess and contact shading, one-dot silhouettes, geometric and authored contours, closed-case seam cleanup, and docked-pod contrast. It also defines the 64-level background-to-ink palette and dot weight.
- [`rendering/Dark.js`](../rendering/Dark.js) owns the original diffuse, specular, and rim illumination, 16-level shadow-to-foreground palette with expanded contrast, and dark dot weight.
- [`rendering/Raster.js`](../rendering/Raster.js) shares projection, triangle coverage, depth testing, interpolated attributes, and optional complete surface layers. It also smooths shallow sub-cell silhouette notches for both themes. It has no theme or pod/case shading decisions.

Both renderers expose `render(scene, view)` and `dotStyle(pitch, background, shadow)`. The existing `DotMesh.renderFrame(...)` and `frame(...)` entry points remain available. The canvas has no theme-specific shading or palette formulas. Every variation uses the selected renderer; feature treatments remain contextual within the light renderer, using surface roles, materials, and authored mesh attributes. This consolidation preserves those treatments rather than replacing them with a single lighting equation.

This is a permanent standalone development app, with no daemon, Bluetooth,
Quickshell session, or connected earbuds required. It needs Qt 6 with Qt Quick
Controls and Layouts. The launcher defaults to `/usr/lib/qt6/bin/qml`; set
`QML_BIN` for another installation. It can be invoked by absolute path from
any working directory.

Keep the preview when integrating the renderer into the live plugin. It imports
the same `DotPods.qml`, `DotMesh.js`, rendering modules, and generated mesh rather
than maintaining a separate renderer. Edit those shared files, restart the preview, and compare
the light/dark, open/closed, and standalone-pod states before trying the plugin.
The original Blender reference, exporter, license notes, and renderer tests are
also retained in the repository. Blender is only needed to regenerate geometry.

The experimental popup integration uses `CaseMesh.qml` to frame this same
renderer for every case state, including either pod out and the empty case. Its camera is fixed
while the hinge and both pods animate; the study retains its rotation controls and all
mesh variants. The canvas renders at twice its displayed dimensions so small
popup dots retain their gaps.

The study is saved in Git as a baseline before live-plugin integration. To run
that version later without reverting ongoing work, use
`git worktree add --detach ../fleetpods-artwork-baseline <study-commit>` and run
that checkout's `tools/artwork/preview`. Find the checkpoint with
`git log --oneline --grep='preserve dot-mesh artwork study'`.

Renderer and component checks, from the repository root:

```sh
deno run --allow-read plugin/tests/dotmesh.test.js
deno run --allow-read plugin/tests/silhouette.test.js
deno run --allow-read plugin/tests/lid-face.test.js
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  /usr/lib/qt6/bin/qmltestrunner -input plugin/tests/tst_DotPods.qml -import plugin/tests/imports
```

The JavaScript checks include 80 exact frame digests covering both themes, all
case/pod states, partial lid openings, and two grid sizes. These originated as
renderer-split parity checks and are updated for intentional artwork changes.
After reviewing an intentional visual change in the preview, update
these snapshots with:

```sh
deno run --allow-read --allow-write=plugin/tests/dotmesh-frames.json \
  plugin/tests/dotmesh.test.js --update-frames
```

To capture a still without opening a desktop window:

```sh
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  ./tools/artwork/preview --capture
```

The capture is written to `/tmp/fleetpods-dotmesh.png`. Add `--light` after `--capture` to capture the light theme to `/tmp/fleetpods-dotmesh-light.png`; `--light` also opens the interactive preview in the light theme. Captures pause at a fixed pose for comparison.

The light theme uses one continuous tonal grid for the case body, outer lid, lid recesses, and earbuds. Every visible sample blends from foreground to the actual background color through the same palette. There is no random dot removal or minimum gray floor; a fully bright surface merges into the background.

A fixed, broad frontal light controls the matte fade around rounded surfaces while well-lit white faces remain clear. Light-theme shading uses diffuse incidence only, without specular highlights, rim lighting, or curvature-threshold bands. Dark material openings retain full tone. A small screen-space ambient-occlusion approximation adds local shading by sampling geometry above each surface tangent plane, ignoring empty background, remote surfaces, and convex self-occlusion. In open-case views, the case assembly and each docked pod are shaded from separate complete depth/normal layers before their visible samples are combined. This keeps the lid’s recess samples intact behind the pods, preventing bright overlap halos. Local shading is evaluated within each layer; it does not simulate shadows cast between pods and the case. It does not reproduce offscreen or hidden shadow casters. Weighted short-range signed-normal samples retain shallow concavities such as the closed case’s front finger recess, even when their depth is smaller than one grid cell. Stronger cavity contrast and a local surface-aware filter give these depressions a continuous tonal falloff over several rows, while rejecting unrelated sharp ledges. Flat and convex surfaces receive no extra recess tone. Each shading layer is depth-tested internally; the final scene depth test chooses which layer is visible. The dark theme keeps its diffuse/specular/rim illumination and maps its useful range through a smooth contrast curve before quantizing to 16 shades. The popup uses a near-background shadow endpoint and larger dots, retaining dark recesses while bringing lit faces toward the foreground color.

In the light-theme open case, docked earbud shells use a paler continuous tone range so their white silhouettes separate from the dark lid recesses and case rim. The shell gradient retains visible intermediate tones through the 64-level palette, fading smoothly into white on lit faces and staying lighter than the recesses. Shell tone comes directly from the volume shading before feature lines are applied, and stays consistent through overlap boundaries. There is no separate white rim or inset contour. Actual foreground pod-to-case boundaries retain a one-dot trace followed directly by the light volume gradient. Generic crease responses on smooth shell curvature are rejected unless supported by a sharp normal bend or a depth break. Silicone tips retain their overlap contour, and their inward-facing bore walls retain dark shading from depth-tested mesh attributes even though the material is white. Silicone-to-shell joins receive a narrow trace, while interior hard edges and deep shadows retain contrast away from the outer profile. Grazing shell contours stay pale; contours against empty background and dark material openings retain their ink. Case shading, standalone earbuds, and the dark theme are unchanged by this contrast treatment.

Silhouettes use cardinal adjacency to trace one-cell contours with connected diagonal staircases. Nearby feature responses are suppressed to avoid a second outline, and neighboring grid dots fade by tone rather than forming another solid outline. Dark vents remain filled even when they meet an outline.

The open lid keeps one pale inner row beside its exposed white lip when viewed
from inside. This preserves the thin plastic separation between the silhouette
and socket shading at popup grid sizes. The pass requires matching lid surfaces,
white material, a front-facing lip normal, and nearby depth; it leaves the outer
outline, deeper sockets, and overlapping pods intact. Dark rendering and closed
cases do not use this treatment.

Small earbud vents retain a soft minimum width in light mode. The rasterizer
measures distance to the projected opening triangles within 0.65 grid cells;
the light renderer uses this partial coverage to preserve rounded capsule ends
when foreshortening would otherwise leave a diagonal line of dots. Coverage
is restricted to nearby white plastic on the same visible pod and is tested
against its final depth. The source mesh and dark rendering are unchanged.
Where a docked vent sits next to a dark lid recess, the visible white pod shell
keeps a pale boundary row near that opening. This separation stays on the pod,
preserves the vent footprint, and does not lighten the lid behind it.

Before either theme shades the grid, shared coverage repair removes shallow
silhouette divots caused by sub-cell geometry, such as the open lid's small
bevel transition. It fills only exterior gaps up to two cells wide and one cell
deep, with actual triangle samples within a quarter cell and matching surface,
material, and nearby depth on three sides. Filled cells retain their sampled
lighting and surface attributes, including the light renderer's complete layers.
The pass does not thicken straight edges or fill enclosed holes, deeper cutouts,
or gaps between different parts/materials. Repairs are evaluated together so one
filled cell cannot grow another repair.

Visible geometric features also receive uninterrupted ink: a surface-normal pass traces bends around the case rim and lid recesses, and part boundaries retain nearly flush seams such as the closed lid. The closed-case join uses one consistent lid-side trace, suppressing duplicate crease responses nearby so depth ties and stair-step corners cannot create knots. The finger-bevel contours are applied afterward and remain separate. These lines use depth-tested surface data, so hidden edges and triangle boundaries are not drawn. Responses are thinned in both screen directions to keep oblique edges connected, with a grid-dependent threshold to retain detail at different dot spacings. The shallow front finger bevel additionally uses two explicit mesh-vertex paths: a one-dot upper contour attached to the lid and a lower contour attached to the body. Their enclosed surfaces retain the tonal gradient while softening the central part seam. Both paths are transformed with their parts and checked against the final depth buffer, so hidden or back-facing bevels do not draw. For standalone earbuds, normal-based lines additionally require a sharp adjacent bend or depth break; smooth stem curvature stays tonal as it moves across the grid. This pass applies only to the light theme.

Add `--open` or `--closed` to start the preview or capture in that case state.
