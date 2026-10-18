# Plugin

Quickshell panel for the Omarchy bar. The manifest at the repository root points `barWidget` at `plugin/Panel.qml`.

The panel reads `$XDG_STATE_HOME/fleetpods/status.json` and sends commands through the `fleetpods` client. It does not link the Rust crate, import anything under `src/`, or talk to BlueZ, PipeWire, or the keyring.

`src/panel/` is the daemon side of that contract: socket verbs and the status snapshot. QML does not go there.

Plugin tests stay under `plugin/tests`. `cargo test` does not run them.

The standalone [dot-mesh artwork study](previews/README.md) is kept alongside
the plugin for visual iteration. Launch it with
`./tools/artwork/preview --light --open` from the repository root; it needs no
running daemon or connected earbuds.

```
deno run --allow-read plugin/tests/model.test.js
deno run --allow-read plugin/tests/replay.test.js
```
