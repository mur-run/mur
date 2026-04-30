// Hotkey rebinder. Click "Change…" → button enters capture mode →
// next keydown captures modifiers + code → calls voice_rebind_hotkey
// → unregisters old combo + registers new + persists to hotkey.json.
//
// Rules:
//   - Esc cancels the capture.
//   - Bare modifier keys (Shift, Ctrl, Cmd, Alt) on their own do not
//     submit; we wait for a non-modifier key.
//   - At least one modifier required (otherwise random keys would
//     intercept normal typing system-wide).

import { useEffect, useRef, useState } from "react";
import { voiceGetHotkey, voiceRebindHotkey } from "./api";
import type { HotkeyConfig } from "./types";

const MODIFIER_NAMES: Record<string, string> = {
  super: "⌘",
  control: "⌃",
  alt: "⌥",
  shift: "⇧",
};

const MODIFIER_CODES = new Set([
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
  "OSLeft",
  "OSRight",
]);

function formatHotkey(cfg: HotkeyConfig): string {
  const order = ["control", "alt", "shift", "super"];
  const mods = order
    .filter((m) => cfg.modifiers.includes(m))
    .map((m) => MODIFIER_NAMES[m] ?? m);
  // Strip "Key" / "Digit" / display Quote as ' for readability.
  let label = cfg.code;
  if (label.startsWith("Key")) label = label.slice(3);
  else if (label.startsWith("Digit")) label = label.slice(5);
  else if (label === "Quote") label = "'";
  else if (label === "Backquote") label = "`";
  else if (label === "Backslash") label = "\\";
  else if (label === "Slash") label = "/";
  else if (label === "Semicolon") label = ";";
  return [...mods, label].join("");
}

export function HotkeyRebinder() {
  const [cfg, setCfg] = useState<HotkeyConfig | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const captureBoxRef = useRef<HTMLDivElement>(null);

  async function refresh() {
    try {
      setCfg(await voiceGetHotkey());
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    if (!capturing) return;
    captureBoxRef.current?.focus();
    const handler = async (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setCapturing(false);
        return;
      }
      if (MODIFIER_CODES.has(e.code)) return; // wait for non-modifier
      const modifiers: string[] = [];
      if (e.metaKey) modifiers.push("super");
      if (e.ctrlKey) modifiers.push("control");
      if (e.altKey) modifiers.push("alt");
      if (e.shiftKey) modifiers.push("shift");
      if (modifiers.length === 0) {
        setErr(
          "At least one modifier is required (otherwise the key would intercept normal typing).",
        );
        return;
      }
      setErr(null);
      try {
        const next = await voiceRebindHotkey(modifiers, e.code);
        setCfg(next);
      } catch (err: unknown) {
        setErr(typeof err === "string" ? err : String(err));
      } finally {
        setCapturing(false);
      }
    };
    window.addEventListener("keydown", handler, { capture: true });
    return () => {
      window.removeEventListener("keydown", handler, { capture: true });
    };
  }, [capturing]);

  if (err) {
    return (
      <div className="text-sm" style={{ color: "var(--color-error, #b91c1c)" }}>
        Hotkey rebinder error: {err}{" "}
        <button
          className="underline ml-2"
          onClick={() => {
            setErr(null);
            refresh();
          }}
        >
          Retry
        </button>
      </div>
    );
  }
  if (!cfg) return <div className="text-sm opacity-60">Loading hotkey…</div>;

  return (
    <div className="flex items-center gap-3 text-sm">
      <span className="opacity-70">PTT hotkey:</span>
      {!capturing && (
        <>
          <code
            className="px-2 py-0.5 rounded font-mono"
            style={{
              background: "var(--color-bg-secondary)",
              color: "var(--color-fg)",
            }}
          >
            {formatHotkey(cfg)}
          </code>
          <button
            onClick={() => {
              setErr(null);
              setCapturing(true);
            }}
            className="px-2 py-0.5 text-xs rounded"
            style={{
              background: "var(--color-accent)",
              color: "var(--color-accent-fg)",
            }}
          >
            Change…
          </button>
        </>
      )}
      {capturing && (
        <div
          ref={captureBoxRef}
          tabIndex={-1}
          className="px-3 py-1 rounded outline-none"
          style={{
            background: "var(--color-bg-secondary)",
            border: "1px dashed var(--color-accent)",
          }}
        >
          Press a new shortcut… <span className="opacity-60 ml-2">(Esc to cancel)</span>
        </div>
      )}
    </div>
  );
}
