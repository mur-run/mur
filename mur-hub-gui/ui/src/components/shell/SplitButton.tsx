import type { ReactNode } from "react";
import { useMenu } from "./useMenu";

export interface MenuItemDef {
  id: string;
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  /** Renders with the danger colour (Delete). */
  danger?: boolean;
}

export interface SplitButtonProps {
  label: ReactNode;
  onPrimary: () => void;
  items: MenuItemDef[];
  disabled?: boolean;
  menuLabel: string;
}

export function MenuList({ items, onPick }: { items: MenuItemDef[]; onPick: () => void }) {
  return (
    <ul className="menu" role="menu">
      {items.map((it) => (
        <li key={it.id} role="none">
          <button
            type="button"
            role="menuitem"
            className={`menu__item${it.danger ? " menu__item--danger" : ""}`}
            disabled={it.disabled}
            onClick={() => {
              onPick();
              it.onSelect();
            }}
          >
            {it.label}
          </button>
        </li>
      ))}
    </ul>
  );
}

/** Primary action + a chevron that opens the alternatives (spec §4.6). */
export function SplitButton({ label, onPrimary, items, disabled, menuLabel }: SplitButtonProps) {
  const { open, setOpen, rootRef } = useMenu();
  return (
    <div className="split" ref={rootRef}>
      <button type="button" className="btn btn--primary split__main" onClick={onPrimary} disabled={disabled}>
        {label}
      </button>
      <button
        type="button"
        className="btn btn--primary split__more"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={menuLabel}
        onClick={() => setOpen(!open)}
        disabled={disabled}
      >
        ▾
      </button>
      {open && <MenuList items={items} onPick={() => setOpen(false)} />}
    </div>
  );
}
