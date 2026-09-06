import { MenuList, type MenuItemDef } from "./SplitButton";
import { useMenu } from "./useMenu";

/** The "⋯" button: secondary actions that do not earn a header button. */
export function OverflowMenu({ items, label }: { items: MenuItemDef[]; label: string }) {
  const { open, setOpen, rootRef } = useMenu();
  return (
    <div className="split" ref={rootRef}>
      <button
        type="button"
        className="btn btn--secondary btn--icon"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        title={label}
        onClick={() => setOpen(!open)}
      >
        ⋯
      </button>
      {open && <MenuList items={items} onPick={() => setOpen(false)} />}
    </div>
  );
}
