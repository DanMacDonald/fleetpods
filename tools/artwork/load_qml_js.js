// Evaluate our function-only QML JavaScript modules in Deno for headless
// geometry checks. Resolve the same relative .import graph used by Qt.
export function loadQmlJs(url, cache = new Map()) {
  if (cache.has(url.href)) return cache.get(url.href)
  const dependencies = {}
  const source = Deno.readTextFileSync(url).replace(
    /^\.import "([^"]+)" as (\w+)\s*$/gm,
    (_, path, name) => {
      dependencies[name] = loadQmlJs(new URL(path, url), cache)
      return ""
    },
  )
  const names = [...source.matchAll(/^function (\w+)\(/gm)].map(match => match[1])
  const module = new Function(...Object.keys(dependencies), source + "; return {" + names.join(",") + "}")(
    ...Object.values(dependencies),
  )
  cache.set(url.href, module)
  return module
}
