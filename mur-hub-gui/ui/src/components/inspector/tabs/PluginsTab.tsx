import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentDetail, InstalledAddonView } from "../../../types";

export function PluginsTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function importPlugin() {
    setError(null);
    setBusy(true);
    try {
      const dir = await open({ directory: true, title: "Select a Claude plugin folder" });
      if (!dir || Array.isArray(dir)) return;
      const updated = await invoke<AgentDetail>("agent_addon_import", {
        name: detail.agent_name,
        pluginDir: dir,
        force: false,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggleAddon(id: string, enabled: boolean) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_addon_toggle", {
        name: detail.agent_name,
        addonId: id,
        enabled,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeAddon(id: string) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_addon_remove", {
        name: detail.agent_name,
        addonId: id,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="tab-form">
      <button
        className="btn btn--sm btn--primary"
        disabled={busy}
        onClick={importPlugin}
        style={{ alignSelf: "flex-start" }}
      >
        Import plugin…
      </button>
      <p className="field-muted" style={{ fontSize: 12 }}>
        Imported plugins start disabled. Enable them after reviewing their contents.
      </p>
      {error && <p className="save-error">{error}</p>}
      {detail.addons.length === 0 && (
        <div className="tab-empty">
          <p className="field-muted">No add-ons imported.</p>
        </div>
      )}
      {detail.addons.length > 0 && (
        <>
          <label className="field-label" style={{ marginTop: 12 }}>
            Installed Add-ons ({detail.addons.length})
          </label>
          <ul className="item-list">
            {detail.addons.map((g: InstalledAddonView) => (
              <li key={g.id} className={g.enabled ? "item-card" : "item-card item-card-off"}>
                <button
                  className="item-card-remove"
                  title="Remove"
                  aria-label="Remove"
                  disabled={busy}
                  onClick={() => removeAddon(g.id)}
                >
                  ×
                </button>
                <label className="item-card-toggle" title={g.enabled ? "Disable" : "Enable"}>
                  <input
                    type="checkbox"
                    checked={g.enabled}
                    disabled={busy}
                    onChange={(e) => toggleAddon(g.id, e.target.checked)}
                  />
                </label>
                <div className="item-card-name">{g.id}</div>
                <div className="item-card-desc">{g.source}</div>
                <span className="field-muted" style={{ fontSize: 11 }}>
                  skills:{g.skills.length} · mcp:{g.mcp.length} · commands:{g.commands.length}
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
