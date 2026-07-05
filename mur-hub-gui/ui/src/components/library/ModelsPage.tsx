import { ModelLibrary } from "../ModelLibrary";

// ─── Component ────────────────────────────────────────────────────────────────

/**
 * Models library page — renders the existing ModelLibrary component inline
 * (embedded mode) instead of as a floating modal.
 */
export function ModelsPage() {
  return (
    <div style={{ height: "100%" }}>
      <ModelLibrary open onClose={() => {}} embedded />
    </div>
  );
}
