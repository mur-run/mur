// Theme is applied via the data-theme attribute on <html>; CSS tokens live in
// styles/tokens/semantic.css ([data-theme="dark"|"light"], else prefers-color-scheme).
export type ThemeChoice = "system" | "light" | "dark";

const STORAGE_KEY = "mur.hub.theme";

/** Attribute value for a choice, or null when the choice defers to the OS. */
export function themeAttr(c: ThemeChoice): "light" | "dark" | null {
  return c === "system" ? null : c;
}

export function getStoredTheme(): ThemeChoice {
  const v = localStorage.getItem(STORAGE_KEY);
  return v === "light" || v === "dark" || v === "system" ? v : "system";
}

export function applyTheme(c: ThemeChoice): void {
  localStorage.setItem(STORAGE_KEY, c);
  const attr = themeAttr(c);
  if (attr) document.documentElement.setAttribute("data-theme", attr);
  else document.documentElement.removeAttribute("data-theme");
}
