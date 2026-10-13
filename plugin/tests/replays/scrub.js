// Turns "fleetpods-replay" journal lines into a replay fixture safe to commit.
// Usage: journalctl --user -o cat ... | deno run plugin/tests/replays/scrub.js > plugin/tests/replays/<name>.jsonl
// Every Bluetooth address maps to a stable 02:00:00:00:00:NN placeholder, keeping its separator and case,
// and personal names are dropped, so no capture carries a real device identity.
// After the first status, each one keeps only the top-level fields that changed ("set", "drop"); replay.test.js
// rebuilds the full line, so a session stays small enough to commit.
const text = new TextDecoder().decode(await new Response(Deno.stdin.readable).arrayBuffer())
const addresses = new Map()
const placeholder = (raw) => {
  const key = raw.toUpperCase().replace(/_/g, ":")
  if (!addresses.has(key)) addresses.set(key, "02:00:00:00:00:" + String(addresses.size + 1).padStart(2, "0"))
  let value = addresses.get(key)
  if (raw.includes("_")) value = value.replace(/:/g, "_")
  return raw === raw.toLowerCase() ? value.toLowerCase() : value
}
const scrub = (s) => s
  .replace(/(?<![0-9A-Fa-f])(?:[0-9A-Fa-f]{2}[:_]){5}[0-9A-Fa-f]{2}(?![0-9A-Fa-f])/g, placeholder)
  .replace(/\b[A-Z][a-z]+(?:’|')s\s+/g, "")
  .replace(/("(?:device_name|peer_name)":")[^"]*"/g, '$1AirPods"')
  .replace(/("id":")[0-9a-fA-F-]{6,}"/g, '$1fleet"')
const out = []
let last = null
for (const line of text.split("\n")) {
  const at = line.indexOf("fleetpods-replay ")
  if (at < 0) continue
  const event = JSON.parse(line.slice(at + "fleetpods-replay ".length))
  if (event.message !== undefined) event.message = scrub(event.message)
  if (event.raw !== undefined) {
    const status = JSON.parse(scrub(event.raw))
    if (last) {
      delete event.raw
      const set = {}, drop = []
      for (const key of Object.keys(status))
        if (JSON.stringify(status[key]) !== JSON.stringify(last[key])) set[key] = status[key]
      for (const key of Object.keys(last)) if (!(key in status)) drop.push(key)
      event.set = set
      if (drop.length) event.drop = drop
    } else event.raw = JSON.stringify(status)
    last = status
  }
  out.push(JSON.stringify(event))
}
const leftover = out.join("\n").match(/(?:[0-9A-Fa-f]{2}[:_]){5}[0-9A-Fa-f]{2}/g)?.filter((a) => !/^02[:_]00[:_]00/i.test(a))
if (leftover?.length) throw new Error("unscrubbed address left: " + leftover.length)
console.log(out.join("\n"))
