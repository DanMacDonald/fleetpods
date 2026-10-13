# Status log replays

Each `.jsonl` file is one recorded panel session: the raw `status.json` lines the shell applied, the seat clicks,
the commands it sent and the ones the daemon refused, and every status log row it appended, rewrote, closed or
cleared. The 2026-10-07 sessions predate the recorded refusals; their `reply` events were inferred from the error
row each refused click produced. `replay.test.js` feeds the
statuses back through the status log and requires the same rows.

To record a session:

1. `python3 plugin/tests/replays/record.py plugin /tmp/fleetpods-recorder` builds recorder copies of three files.
2. Copy them over the deployed plugin and restart the shell. Each event is logged as one `fleetpods-replay` line.
3. Run the scenario, then collect and scrub it:
   `journalctl --user -o cat --since <start> | grep fleetpods-replay | deno run plugin/tests/replays/scrub.js > plugin/tests/replays/<name>.jsonl`
4. Redeploy the normal plugin.

The scrub maps every Bluetooth address to a `02:00:00:00:00:NN` placeholder and drops personal device names. It
refuses to write a fixture with any other address left in it. Never commit a capture that has not been through it.

## Reviewed expectations (2026-10-13)

The recordings are unchanged. The adjacent changes files document each intentional log difference.
The replay checks every status boundary, including boundaries following recorded row changes.
A `start` rule moves the beginning of a connection row to admission; `amend` advances that same
row before the socket snapshot arrives. Both preserve later recorded rewrites and completion.

The seat snapshots were reviewed against the recorded inputs and updated for these changes:

- Admission starts the transition immediately. Stale rosters cannot return a released helper to its seat.
- A local laptop occupies its seat after control setup is ready, rather than at the first BlueZ connection report.
- A route outcome announces switching; the confirmed roster announces completion.
- A finished route cannot start a new switching animation after an unrelated disconnect.
- Without current connection evidence, remembered Apple devices return to the tray.
- Case closure changes the subtitle immediately; the disconnected local observer shows lid guidance.

These recordings include failures and expired evidence from an older daemon. Preserving their behavior as
regression inputs does not make them successful hardware handoff tests. Focused model and QML tests cover
the current transition rules.

Run `deno run --allow-read plugin/tests/replay.test.js` to compare expectations. After reviewing intentional
seat changes, `--allow-write=plugin/tests/replays` plus `--write-seats` updates the seat snapshots.
Log expectations always require explicit annotations.
