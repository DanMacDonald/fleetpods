# Dot mesh artwork study

Run the interactive preview from the repository root:

```sh
./tools/artwork/preview --light --open
```

Drag the large artwork or use the rotation slider. Switch between earbuds, an open case, and a closed case; pause the motion, adjust the grid spacing, or switch the background. The bottom row shows the artwork at the popup's 128-pixel size.

This study uses a simplified CC0 AirPods Pro reference mesh, with separate case and lid geometry and mirrored earbuds. See [asset provenance and regeneration](../artwork/README.md). `DotMesh.js` projects and depth-tests its triangles onto a fixed grid. `DotPods.qml` draws that grid with square dots: eight lighting levels for dark artwork and 64 tone levels for every light-theme surface. It runs at 20 frames per second while visible; hiding or pausing it stops the timer. Colors are component inputs.

This is a permanent standalone development app, with no daemon, Bluetooth,
Quickshell session, or connected earbuds required. It needs Qt 6 with Qt Quick
Controls and Layouts. The launcher defaults to `/usr/lib/qt6/bin/qml`; set
`QML_BIN` for another installation. It can be invoked by absolute path from
any working directory.

Keep the preview when integrating the renderer into the live plugin. It imports
the same `DotPods.qml`, `DotMesh.js`, and generated mesh rather than maintaining
a separate renderer. Edit those shared files, restart the preview, and compare
the light/dark, open/closed, and standalone-pod states before trying the plugin.
The original Blender reference, exporter, license notes, and renderer tests are
also retained in the repository. Blender is only needed to regenerate geometry.

The study is saved in Git as a baseline before live-plugin integration. To run
that version later without reverting ongoing work, use
`git worktree add --detach ../fleetpods-artwork-baseline <study-commit>` and run
that checkout's `tools/artwork/preview`. Find the checkpoint with
`git log --oneline --grep='preserve dot-mesh artwork study'`.

Renderer and component checks, from the repository root:

```sh
deno run --allow-read plugin/tests/dotmesh.test.js
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  /usr/lib/qt6/bin/qmltestrunner -input plugin/tests/tst_DotPods.qml -import plugin/tests/imports
```

To capture a still without opening a desktop window:

```sh
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  ./tools/artwork/preview --capture
```

The capture is written to `/tmp/fleetpods-dotmesh.png`. Add `--light` after `--capture` to capture the light theme to `/tmp/fleetpods-dotmesh-light.png`; `--light` also opens the interactive preview in the light theme. Captures pause at a fixed pose for comparison.

The light theme uses one continuous tonal grid for the case body, outer lid, lid recesses, and earbuds. Every visible sample blends from foreground to the actual background color through the same palette. There is no random dot removal or minimum gray floor; a fully bright surface merges into the background.

A fixed, broad frontal light controls the matte fade around rounded surfaces while well-lit white faces remain clear. Light-theme shading uses diffuse incidence only, without specular highlights, rim lighting, or curvature-threshold bands. Dark material openings retain full tone. A small screen-space ambient-occlusion approximation adds contact shading by sampling visible geometry above each surface tangent plane, ignoring empty background, remote surfaces, and convex self-occlusion. It does not reproduce offscreen or hidden shadow casters. Weighted short-range signed-normal samples retain shallow concavities such as the closed case’s front finger recess, even when their depth is smaller than one grid cell. Stronger cavity contrast and a local surface-aware filter give these depressions a continuous tonal falloff over several rows, while rejecting unrelated sharp ledges. Flat and convex surfaces receive no extra recess tone. All shading is computed after depth testing. The dark theme retains its original illumination, palette, and dot weight.

In the light-theme open case, docked earbud shells use a paler continuous tone range so their white silhouettes separate from the dark lid recesses and case rim. Shell tone comes directly from the volume shading before feature lines are applied, and stays consistent through overlap boundaries. There is no separate white rim or inset contour. Silicone tips retain a narrow overlap contour, and their inward-facing bore walls retain dark shading from depth-tested mesh attributes even though the material is white. Silicone-to-shell joins receive a narrow trace, while interior hard edges and deep shadows retain contrast away from the outer profile. Grazing shell contours stay pale; contours against empty background and dark material openings retain their ink. Case shading, standalone earbuds, and the dark theme are unchanged by this contrast treatment.

Silhouettes use cardinal adjacency to trace one-cell contours with connected diagonal staircases. Nearby feature responses are suppressed to avoid a second outline, and neighboring grid dots fade by tone rather than forming another solid outline. Dark vents remain filled even when they meet an outline.

Visible geometric features also receive uninterrupted ink: a surface-normal pass traces bends around the case rim and lid recesses, and part boundaries retain nearly flush seams such as the closed lid. These lines use depth-tested surface data, so hidden edges and triangle boundaries are not drawn. Responses are thinned in both screen directions to keep oblique edges connected, with a grid-dependent threshold to retain detail at different dot spacings. The shallow front finger bevel additionally uses two explicit mesh-vertex paths: a one-dot upper contour attached to the lid and a lower contour attached to the body. Their enclosed surfaces retain the tonal gradient while softening the central part seam. Both paths are transformed with their parts and checked against the final depth buffer, so hidden or back-facing bevels do not draw. This pass applies only to the light theme.

Add `--open` or `--closed` to start the preview or capture in that case state.
