import { getCurrentWindow } from "@tauri-apps/api/window";

export type ThemePref = "system" | "light" | "dark";

const KEY = "rv-theme";

function isPref(v: unknown): v is ThemePref {
  return v === "system" || v === "light" || v === "dark";
}

/** Last theme the user picked, kept outside settings.json so it applies before the first paint. */
export function cachedTheme(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return isPref(v) ? v : "system";
  } catch {
    return "system";
  }
}

function paint(pref: ThemePref) {
  const light = pref === "light" || (pref === "system" && window.matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
}

/** Apply a theme now and keep following macOS while it is "system". Returns a cleanup. */
export function applyTheme(pref: ThemePref, native: boolean): () => void {
  try {
    localStorage.setItem(KEY, pref);
  } catch {
    // Private storage unavailable: the theme still applies for this session.
  }
  paint(pref);
  if (native) {
    // The window follows macOS when the theme is null, which also drives prefers-color-scheme.
    getCurrentWindow()
      .setTheme(pref === "system" ? null : pref)
      .then(() => paint(pref))
      .catch(() => {});
  }
  if (pref !== "system") return () => {};
  const mq = window.matchMedia("(prefers-color-scheme: light)");
  const onChange = () => paint("system");
  mq.addEventListener("change", onChange);
  return () => mq.removeEventListener("change", onChange);
}
