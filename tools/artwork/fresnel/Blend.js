// Overlay brighter baseline dots while retaining the Fresnel edge reflections.
// Input colors are the rendered palette colors, not unrelated intensity ramps.
function combine(fresnel, baseline, amount) {
  var mix = Math.max(0, Math.min(1, amount))
  return fresnel.map(function(value, channel) {
    return value + Math.max(0, baseline[channel] - value) * mix
  })
}
