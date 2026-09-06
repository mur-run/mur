/** macOS gets the overlay title bar (traffic lights inside the sidebar);
 *  other platforms keep native decorations, so their inset is 0. */
export function isMac(): boolean {
  return typeof navigator !== "undefined" && /Mac/.test(navigator.platform);
}
