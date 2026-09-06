import { useEffect, useState, type KeyboardEvent } from "react";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { rankPalette, type PaletteItem } from "./palette";

/** ⌘K (or Ctrl+K) opens the palette. */
export function isPaletteShortcut(e: globalThis.KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k";
}

/** Jump to any agent / fleet / page, or run an action on the current
 *  selection (spec §6.6). The caller builds `items`; this only ranks and picks. */
export function CommandPalette({ open, items, onClose }: { open: boolean; items: PaletteItem[]; onClose: () => void }) {
  const { t } = useT();
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const visible = rankPalette(items, query);

  useEffect(() => {
    if (open) {
      setQuery("");
      setCursor(0);
    }
  }, [open]);

  if (!open) return null;

  function pick(it: PaletteItem | undefined) {
    if (!it) return;
    onClose();
    it.run();
  }
  function onKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((c) => Math.min(visible.length - 1, c + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => Math.max(0, c - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      pick(visible[cursor]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  }

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div className="palette" role="dialog" aria-label={t("palette.title")} onMouseDown={(e) => e.stopPropagation()}>
        <input
          autoFocus
          className="palette__input"
          placeholder={t("palette.placeholder")}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setCursor(0);
          }}
          onKeyDown={onKey}
        />
        <ul className="palette__list" role="listbox">
          {visible.length === 0 && <li className="palette__empty">{t("palette.empty")}</li>}
          {visible.map((it, i) => (
            <li
              key={it.id}
              role="option"
              aria-selected={i === cursor}
              className={`palette__item${i === cursor ? " palette__item--on" : ""}`}
              onMouseEnter={() => setCursor(i)}
              onClick={() => pick(it)}
            >
              <span className="palette__kind">{t(`palette.kind.${it.kind}` as TranslationKey)}</span>
              <span className="palette__label">{it.label}</span>
              {it.hint && <span className="palette__hint">{it.hint}</span>}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
