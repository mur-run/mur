import { useEffect, useState } from "react";

/// Full-window dashed-border overlay that appears whenever the user
/// is dragging files over the window. Pure visual — actual file
/// processing fires from `App.tsx`'s `multimodal://drop` Tauri
/// listener. Uses a counter so nested dragenter/dragleave events
/// (which Webkit fires for every child element) don't flicker the
/// overlay.
export function DropOverlay() {
  const [active, setActive] = useState(false);

  useEffect(() => {
    let counter = 0;
    const enter = (e: DragEvent) => {
      e.preventDefault();
      counter += 1;
      if (counter === 1) setActive(true);
    };
    const over = (e: DragEvent) => e.preventDefault();
    const leave = (_e: DragEvent) => {
      counter -= 1;
      if (counter <= 0) {
        counter = 0;
        setActive(false);
      }
    };
    const drop = (e: DragEvent) => {
      e.preventDefault();
      counter = 0;
      setActive(false);
    };
    window.addEventListener("dragenter", enter);
    window.addEventListener("dragover", over);
    window.addEventListener("dragleave", leave);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragenter", enter);
      window.removeEventListener("dragover", over);
      window.removeEventListener("dragleave", leave);
      window.removeEventListener("drop", drop);
    };
  }, []);

  if (!active) return null;
  return (
    <div
      role="presentation"
      className="fixed inset-0 z-40 flex items-center justify-center pointer-events-none"
      style={{ background: "rgba(0,0,0,0.35)" }}
    >
      <div
        className="rounded-lg p-8 text-center"
        style={{
          border: "2px dashed var(--color-accent)",
          background: "var(--color-bg)",
          color: "var(--color-fg)",
        }}
      >
        <div className="text-lg font-medium">Drop files to attach</div>
        <div className="text-sm opacity-75 mt-1">
          Up to 10 files, 30 MB total. Images and PDFs.
        </div>
      </div>
    </div>
  );
}
