// deno run --allow-read --allow-write --allow-run=rsvg-convert tools/artwork/logo.js
// Draws the FleetPods logo with the panel's own dot-mesh renderer and default
// dark palette, so it matches the popup artwork. Writes the README logo and the
// GitHub social preview (Settings > Social preview) to docs/images/.
import { loadQmlJs } from './load_qml_js.js'

const root = new URL('../../', import.meta.url)
const Mesh = loadQmlJs(new URL('plugin/DotMesh.js', root))
const geometry = new Function(Deno.readTextFileSync(new URL('plugin/artwork/AirPodsMesh.js', root))
  .replace(/^\.pragma library\s*/, '') + '; return geometry')()

// DotPods.qml defaults; the foreground is lifted as the panel lifts it on dark backgrounds.
const background = '#1c1d26', foreground = '#fbfaf4', shadow = '#465675', muted = '#9aa3c7'
const rgb = hex => ({ r: parseInt(hex.slice(1, 3), 16) / 255, g: parseInt(hex.slice(3, 5), 16) / 255,
  b: parseInt(hex.slice(5, 7), 16) / 255 })
const css = c => '#' + c.map(v => Math.round(Math.max(0, Math.min(1, v)) * 255).toString(16).padStart(2, '0')).join('')

// The pair at rest, turned as the popup shows it, as square dots on a grid.
function pods(size, cells, x0, y0) {
  const pitch = size / cells
  const frame = Mesh.renderFrame(geometry, '', 1, 0, -0.32, 0.24, cells, cells, false, 0, 1, 0, 1, true, null)
  const style = Mesh.dotStyle(false, pitch, rgb(background), rgb(foreground), rgb(shadow))
  const dot = Math.min(pitch * 0.85, style.size * 1.4)
  // One path per shade and coverage keeps the file small; coverage comes in quarters.
  const shapes = new Map(), d = dot.toFixed(2)
  for (let i = 0; i < frame.values.length; i++) {
    const band = style.level(frame.values[i])
    if (!band) continue
    const x = x0 + (i % cells + 0.5) * pitch - dot / 2, y = y0 + (Math.floor(i / cells) + 0.5) * pitch - dot / 2
    const key = css(style.colors[band]) + ' ' + (frame.coverage ? frame.coverage[i] : 1)
    if (!shapes.has(key)) shapes.set(key, [])
    shapes.get(key).push(`M${x.toFixed(1)} ${y.toFixed(1)}h${d}v${d}h-${d}z`)
  }
  return [...shapes].map(([key, parts]) => {
    const [fill, alpha] = key.split(' ')
    return `<path fill="${fill}"${alpha < 1 ? ` fill-opacity="${alpha}"` : ''} d="${parts.join('')}"/>`
  }).join('\n')
}

function logo() {
  const size = 512
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${size}" height="${size}">
<title>FleetPods</title>
<rect width="${size}" height="${size}" rx="112" fill="${background}"/>
${pods(580, 120, -19.5, -41)}
</svg>
`
}

function social() {
  const w = 1280, h = 640
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}">
<title>FleetPods</title>
<rect width="${w}" height="${h}" fill="${background}"/>
${pods(640, 132, 27, -9.5)}
<text x="660" y="315" fill="${foreground}" font-family="JetBrains Mono, monospace" font-size="92" font-weight="700">FleetPods</text>
<text x="664" y="385" fill="${muted}" font-family="JetBrains Mono, monospace" font-size="30">Your AirPods, on every</text>
<text x="664" y="427" fill="${muted}" font-family="JetBrains Mono, monospace" font-size="30">computer you own.</text>
</svg>
`
}

Deno.writeTextFileSync(new URL('docs/images/fleetpods-logo.svg', root), logo())
// The preview's title needs JetBrains Mono at render time, so ship it as PNG.
const svg = await Deno.makeTempFile({ suffix: '.svg' })
Deno.writeTextFileSync(svg, social())
const png = new URL('docs/images/fleetpods-social.png', root).pathname
const result = await new Deno.Command('rsvg-convert', { args: [svg, '-o', png] }).output()
Deno.removeSync(svg)
if (!result.success) throw new Error(new TextDecoder().decode(result.stderr))
