import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { LibrarySelection } from "../inspector/LibraryInspector";

// ─── Backend shape ───────────────────────────────────────────────────────────

interface WorkflowView {
  name: string;
  description: string;
  path: string;
}

// Note: no discover/install section here — workflows arrive in
// `~/.mur/workflows/` automatically (relay-installed or authored locally).
// A server-side shared-registry discovery view is a later concern.

// ─── Component ────────────────────────────────────────────────────────────────

export function WorkflowsPage({ onSelect }: { onSelect?: (item: LibrarySelection | null) => void }) {
  const { t } = useT();
  const [workflows, setWorkflows] = useState<WorkflowView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  function pick(w: WorkflowView) {
    setSelected(w.path);
    onSelect?.({ kind: "workflow", name: w.name, origin: w.path, description: w.description });
  }

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<WorkflowView[]>("workflows_list")
      .then((res) => {
        setWorkflows(res);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const openFolder = useCallback((path: string) => {
    invoke("reveal_in_finder", { path }).catch((e) => setError(String(e)));
  }, []);

  return (
    <div>
      {error && <p className="save-error">{error}</p>}

      {loading ? (
        <p className="field-muted">{t("workflowslib.loading")}</p>
      ) : workflows.length === 0 ? (
        <div className="tab-empty">
          <p>{t("workflowslib.empty")}</p>
        </div>
      ) : (
        <ul className="item-list">
          {workflows.map((w) => (
            <li
              key={w.path}
              className={`item-card${selected === w.path ? " item-card--selected" : ""}`}
              onClick={() => pick(w)}
              style={{ cursor: "pointer" }}
            >
              <div
                className="item-card-name"
                style={{ display: "flex", gap: 6, alignItems: "center", justifyContent: "space-between" }}
              >
                <span style={{ fontWeight: 600 }}>{w.name}</span>
                <button
                  className="btn btn--sm btn--secondary"
                  onClick={(e) => { e.stopPropagation(); openFolder(w.path); }}
                >
                  {t("workflowslib.openFolder")}
                </button>
              </div>
              <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--text-muted, #888)" }}>
                {w.description}
              </p>
              <p style={{ margin: "2px 0 0", fontSize: 11, color: "var(--text-muted, #888)" }}>
                {w.path}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
