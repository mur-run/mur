import { useEffect, type RefObject } from "react";

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function focusables(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE));
}

/** Make `ref` a real modal for keyboard users: everything else under
 *  `<body>` becomes inert, focus moves inside and cannot Tab out, and on
 *  unmount the background is restored and focus returns to the opener.
 *
 *  The dialog must be rendered as a direct child of `<body>` (a portal) so
 *  that the app root can be made inert without making the dialog inert too.
 *  The container should carry `tabIndex={-1}`: when every control inside is
 *  disabled (e.g. mid-operation), focus parks on the container itself. */
export function useModalFocus(ref: RefObject<HTMLElement | null>) {
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const portal = dialog.closest("body > *");

    // Inert every sibling of the dialog's portal, remembering who already was.
    const made: Element[] = [];
    for (const el of Array.from(document.body.children)) {
      if (el === portal || el.hasAttribute("inert")) continue;
      el.setAttribute("inert", "");
      made.push(el);
    }

    (focusables(dialog)[0] ?? dialog).focus();

    function onKey(e: KeyboardEvent) {
      if (e.key !== "Tab" || !dialog) return;
      const items = focusables(dialog);
      const active = document.activeElement;
      if (items.length === 0) {
        e.preventDefault();
        dialog.focus();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const inside = active instanceof Node && dialog.contains(active);
      if (!inside) {
        e.preventDefault();
        (e.shiftKey ? last : first).focus();
      } else if (e.shiftKey && (active === first || active === dialog)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }
    document.addEventListener("keydown", onKey, true);

    return () => {
      document.removeEventListener("keydown", onKey, true);
      for (const el of made) el.removeAttribute("inert");
      if (opener?.isConnected) opener.focus();
    };
  }, [ref]);
}
