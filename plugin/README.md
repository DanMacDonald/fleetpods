# Plugin

Quickshell panel for the Omarchy bar. The manifest at the repository root points `barWidget` at `plugin/Panel.qml`.

The panel reads `$XDG_STATE_HOME/fleetpods/status.json` and sends commands through the `fleetpods` client. It does not link the Rust crate, import anything under `src/`, or talk to BlueZ, PipeWire, or the keyring.

`src/panel/` is the daemon side of that contract: socket verbs and the status snapshot. QML does not go there.

Plugin tests stay under `plugin/tests`. `cargo test` does not run them.

The panel shows one smaller dot-rendered accessory to the left of the selected
pair. Cases need a fresh lid report; Max headsets need an advertisement from the
last two minutes. Open cases take priority, then nearby headsets, then closed
cases, with stable address order breaking ties. The artwork fades out when its
evidence expires or the daemon becomes unreachable; cached charge and last-known
placement alone do not keep it visible. This display never selects or connects
the secondary accessory.
Both primary and secondary Max artwork use the on-head pose only for an explicit
worn report. Unknown wear uses the resting pose without inventing an off-head
report or changing audio behavior.
New secondary accessories slide in from the left while the primary artwork's
own animation continues. Reports for the same accessory update its pose without
restarting that entrance. The case label follows the painted bottom edge.
When selection changes, the outgoing primary first shrinks and fades straight
back from its fixed center over 200 ms, with the secondary held in place. Then
the incoming secondary slides to the center, growing and brightening, while the
former primary can enter the secondary slot from the left. One animation owner
sequences these phases and reconciles selection, pose and inventory together
before drawing. Promotion suppresses the primary's downward entrance. Returning
to the old selection during retreat cancels the swap; a new selection during the
incoming pan settles immediately. Closing the panel settles the latest selection.
The primary renderer reads only the committed presentation pose, so incoming
status cannot replace it before retreat completes. Scale transitions use a
filtered texture with mipmaps; resting artwork returns to its native dot grid.
In dark themes, the secondary case reserves a bright dot along each column of
the open lid's visible top contour, preserving the thin rim at this smaller size.

The standalone [dot-mesh artwork study](previews/README.md) is kept alongside
the plugin for visual iteration. Launch it with
`./tools/artwork/preview --light --open` from the repository root; it needs no
running daemon or connected earbuds.

The popup uses that renderer for
all case states: closed, both docked, either pod out, and briefly empty when the
second pod leaves. `MeshArtwork.qml` fits the mesh to the existing artwork bounds,
with a fixed camera and a 350 ms hinged lid animation. The same instance survives
case-state changes and reversals during animation. Each pod lifts vertically
clear of the artwork over 840 ms and returns at constant speed over 500 ms, starting
fully above the artwork's clip and entering stem-first. Each side turns independently
from a viewer-facing pose, finishes turning with the stem about halfway into
its well, then descends straight to its seat. An explicit
available `in_case: false` report or an in-ear report starts that side's departure;
missing placement data preserves each pod's last known in-case/out-of-case state,
defaulting to seated when no placement has been observed. This display history
survives connection loss and resets when the daemon disappears or the accessory
is unpaired. A later in-ear event does not
restart an existing lift. If the case closes, returning pods seat before the lid
closes. After the empty-case hold and both pods clearing the artwork, the empty
case slides down through the bottom clip over 350 ms. A returning pod cancels
that exit and restores the case. `MeshArtwork.qml` then presents the standalone pair:
they descend at constant speed over 494 ms with tips initially inward, rotating around their
vertical axes into an outward-facing pose similar to the old illustration.
The connector endpoints stay fixed while the artwork moves over them.
The icon retains both pods' seating poses while that pair replaces the case,
so either pod animates back into an empty case when its renderer loads again.
Bar glyphs are unchanged. The popup palette selects the light or dark renderer,
and settled artwork does not run an animation timer. Preview and scrub the
entrance with `./tools/artwork/preview --pods-out` (also supports `--light`).
All 3D popup states share the 3 px dot pitch and glyph weight in `DotGrid.js`.
The light standalone pods use softer white-shell contours and volume shading
than the case, while retaining dark vents and the same dot size.
Dots align to physical display pixels, with consistent partial coverage at 100%; artwork framing changes the mesh size,
not the dot size or spacing. Preview controls can override the pitch.
Dark rendering uses the approved baseline-study settings: frontal fill 0.57
and face brightness 0.73× on white surfaces, for both pods and case states.
Matte recesses and dark materials retain their original shading. The Fresnel
experiment and its overlay remain confined to the separate art study.
Dark artwork and selected device icons now lift the theme foreground brightness
by 16%, with lit dot tones reaching that endpoint sooner. Geometry, material
lighting, dot spacing, and theme hues stay the same. `highlighted` defaults to
true on the mesh components and `AirPodsIcon`; false fades the dots to 62%
opacity over 180 ms. The popup currently uses the highlighted state throughout.
Light-theme artwork keeps its original palette, with the same optional dim state.

```
deno run --allow-read plugin/tests/model.test.js
deno run --allow-read plugin/tests/replay.test.js
```

`MeshArtwork.qml` is the shared framing interface for both case and standalone
poses. `AirPodsIcon.qml` owns their presentation lifecycle, including the
250 ms last-departure confirmation and the empty-case hold. `Service.qml`
forwards physical placement without scheduling artwork timers, and `SeatsView`
reads the icon's connector positions without inspecting motion flags. Rendering
motion tests
exercise placement changes through the icon.
