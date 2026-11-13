# Connection notification prototype

FleetPods publishes a transient native Omarchy notification while acquiring
playback. The plugin reads `connection_lifecycle`, ownership intent, transport
activity and battery from the daemon's existing status. It never requests a
connection, reports desktop activity, changes routes or controls playback.
Passive background control links and startup during established playback stay quiet.
A request by this laptop starts the toast at fleet admission only when it also
wants audio: ownership intent is `claim` or `want_omarchy_seat` records a queued
claim. `Model.acquiringPlayback` is the one definition of that condition, built
from the same stage lists as the status-bar spinner; background
joins caused by desktop activity stay quiet. A playback claim during a pending
request starts the toast immediately. Helping another laptop get admitted does
not start a local toast.
Status changes are processed immediately. A new playback claim also triggers
the toast if the first observed state is already streaming, so fast fleet
takeovers cannot disappear between timer checks. An existing claim survives
ordinary pause/resume without repeating the toast.

`ConnectionToast.js` owns the presentation lifetime and the single animation
timeline. Its pure `step` consumes daemon status projected for display, time,
and native attachment/dismissal/failure observations. `presentation` samples
the resulting state into one immutable snapshot for text, battery and artwork.
The Playing clock starts only once an overlay is attached; stale publication
feedback cannot affect a newer episode. Retries update one
notification; dismissing it suppresses that attempt. A brief return to routing
keeps the same attempt and restarts its completion hold. Confirmed streaming shows
completion for at least one second and waits for the artwork's half-second rear
hold, one-second turn and one-second final-pose hold before dismissing.
Waiting for playback stops after 10 seconds, and an
attempt never keeps the notification longer than 30 seconds. These are display
limits only; the daemon's connection lifecycle is unchanged.

The model also owns the preview's scripted input and its single end path, and
takes the widget setting as an input: a disabled toast keeps its baseline, so
enabling it later does not announce established playback. Its `LIMIT_MS` is
both the 30-second display limit and the native card's expiry.

`ConnectionNotification.qml` elects one publisher across the per-monitor panels,
samples status and time, and records diagnostics. Its single timer
samples at 30 fps during the turn and four times per second otherwise.
It calls the model's `advance` to receive the next snapshot. The shared model
retains the attempt baseline when the publishing panel disappears, preventing
monitor changes from reannouncing the same attempt.
`NativeNotification.qml` owns publication, replacement, card discovery, overlay
attachment and dismissal. Its interface is `sync(presentation, now)`,
`observation(now)` and `diagnostics()`. All current/legacy Omarchy layout details
live in that adapter; native tree discovery remains limited to four times per
second. Process replies report facts rather than mutating the toast lifecycle.
`NotificationOverlay.qml` only draws the snapshot, freezing it with one assignment
when retired. The native card continues to own its exit animation and destruction.

The native service owns layout, stacking, expiration, DND and input. The
FleetPods overlay is a mouse-transparent child of its card, covering the native
content while preserving the border and close button. It updates phase text
and rings in place. Once attached, the native placeholder
is not republished during phase changes, avoiding native replacement animations.
Only the plain-notification fallback receives native content updates.
The overlay inherits card movement
and visibility and yields to native redaction. It uses the native notification
palette, FleetPods device artwork, and a battery ring on the right with its
percentage centered inside. Unknown charge hides the battery ring. Removing FleetPods removes the additions;
no native plugin is copied, patched or replaced.
The device artwork reuses the panel's dot painter and Max/buds meshes, sized to
fit inside the progress ring throughout its rotation. During connection,
preparation and routing the mesh stays still while the progress ring spins.
Max starts facing backward; earbuds start in the panel's front display pose.
Only “Playing” starts the artwork clock. After a half-second hold, Max makes one
counterclockwise half-turn to face front; each earbud instead turns about its own
vertical axis until its tip faces the other bud. The buds ease slightly apart
and back along the view direction during the turn, leaving a gap between the
tips while keeping the pair centered inside the spinner.
The turn takes one second, followed by a one-second final hold. Cubic ease-in/out gives the turn
gentle endpoints and a quick, symmetric middle, stopping at the final pose.
The single
publisher clock drives every monitor and the completion gate. The turn plays
once per toast when playback is confirmed; later routing fluctuations do not
restart it. Dismissal waits for the complete turn and final hold.
Rendering is limited to about 30 frames per second and
hidden cards skip painting. Reduced motion presents the final pose immediately.
The final pose is held through dismissal; a longer connection stays visible
until playback is confirmed or the existing timeout/failure rules apply.
At dismissal, the overlay freezes its final content and remains owned by the
native card until that card is destroyed. Its background continues covering
the placeholder while the foreground follows native content fades, preventing
the initial wording from showing through during stacking or departure.

This is deliberately an experimental adapter, not a supported Omarchy extension
API. The ordinary plugin facade does not expose cards. The adapter reads the
host's shared QML creation context, looks up its notification service, and walks
its surface/card tree to find only the notification ID published by FleetPods.
It adds its own children without changing existing objects' properties.
The older renderer used by Gram is supported through its `popupModel` and
`NotificationCard` surface; that path uses the native `dismissPopup` method and
retains its native right-click dismissal. Both versions share the same FleetPods
artwork, battery ring and notification lifecycle.
The current renderer retains visible cards after a D-Bus close, so FleetPods
uses the renderer's existing `closeToast` method when available. If those
internals change, the fallback is an ordinary headphones notification with a
30-second native timeout; custom artwork, immediate dismissal and dismissal
tracking can be unavailable. Capability checks contain this dependency here.

Preview without affecting audio:

```sh
omarchy-shell fleetpods connectionToastPreview
omarchy-shell fleetpods connectionToastState
```

The preview simulates connecting, preparing, routing and playing, then dismisses.
Playback is delayed to verify that the mesh stays still during setup and only
turns once “Playing” appears.
It releases control back to live status immediately when its card closes.
It uses the selected device's name and available battery reading. The
`connectionNotifications` FleetPods widget setting disables notifications.
`connectionToastState` reports the current phase, visibility, preview marker and
the adapter's publication and overlay counts.

Validation:

```sh
deno run --allow-read plugin/tests/connection-toast.test.js
deno run --allow-read plugin/tests/model.test.js
deno run --allow-read plugin/tests/replay.test.js
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input plugin/tests/tst_NotificationOverlay.qml -import plugin/tests/imports
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input plugin/tests/tst_NativeNotification.qml -import plugin/tests/imports
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input plugin/tests/tst_ConnectionNotification.qml -import plugin/tests/imports
```

The QML test imports replace Quickshell process execution with controlled replies,
so adapter tests exercise both native layouts, fallback, dismissal and stale
success/failure callbacks without sending real notifications.

Also exercise a preview alongside a native notification, manual dismissal,
monitor changes and shell/plugin reload. The preview must publish one card,
track native stacking and remove its artwork with its card, with no QML errors.
