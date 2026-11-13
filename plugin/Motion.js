.pragma library

// Older Omarchy releases predate Style.duration. Keep their normal timing;
// newer releases retain the shell's reduced-motion preference and updates.
function duration(style, milliseconds) {
  return typeof style.duration === "function" ? style.duration(milliseconds) : milliseconds
}
