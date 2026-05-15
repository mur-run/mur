import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface Props {
  isOpen: boolean;
  onClose: () => void;
}

type Mode = "file" | "url";

export function PresetImportModal({ isOpen, onClose }: Props) {
  const [mode, setMode] = useState<Mode>("file");
  const [url, setUrl] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  if (!isOpen) return null;

  function reset() {
    setUrl("");
    setStatus(null);
    setError(null);
    setLoading(false);
  }

  function handleClose() {
    reset();
    onClose();
  }

  async function importFile() {
    setLoading(true);
    setError(null);
    setStatus(null);
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "YAML preset", extensions: ["yaml", "yml"] }],
      });
      if (!selected) {
        setLoading(false);
        return;
      }
      const id = await invoke<string>("import_preset_file", {
        path: typeof selected === "string" ? selected : selected[0],
      });
      setStatus(`Imported preset "${id}"`);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  async function importUrl() {
    if (!url.trim()) return;
    setLoading(true);
    setError(null);
    setStatus(null);
    try {
      const id = await invoke<string>("import_preset_url", { url: url.trim() });
      setStatus(`Imported preset "${id}"`);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={handleClose}>
      <div
        className="modal-panel"
        style={{ maxWidth: 440 }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <h2>Import Style Preset</h2>
          <button className="modal-close" onClick={handleClose}>×</button>
        </div>

        <div className="modal-body">
          <div className="tab-row" style={{ display: "flex", gap: 8, marginBottom: 16 }}>
            <button
              className={`toolbar-btn${mode === "file" ? " active" : ""}`}
              onClick={() => setMode("file")}
            >
              From File
            </button>
            <button
              className={`toolbar-btn${mode === "url" ? " active" : ""}`}
              onClick={() => setMode("url")}
            >
              From URL
            </button>
          </div>

          {mode === "file" ? (
            <div>
              <p style={{ marginBottom: 12, color: "var(--text-secondary, #888)", fontSize: 13 }}>
                Select a <code>.yaml</code> file containing a StylePreset definition.
                It will be copied to <code>~/.mur/hub/presets/</code>.
              </p>
              <button
                className="toolbar-btn"
                onClick={importFile}
                disabled={loading}
              >
                {loading ? "Importing…" : "Choose File…"}
              </button>
            </div>
          ) : (
            <div>
              <p style={{ marginBottom: 8, color: "var(--text-secondary, #888)", fontSize: 13 }}>
                Enter a URL pointing to a StylePreset YAML file.
              </p>
              <input
                type="url"
                placeholder="https://example.com/my-preset.yaml"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && importUrl()}
                style={{ width: "100%", marginBottom: 8 }}
              />
              <button
                className="toolbar-btn"
                onClick={importUrl}
                disabled={loading || !url.trim()}
              >
                {loading ? "Fetching…" : "Import from URL"}
              </button>
            </div>
          )}

          {status && (
            <p style={{ marginTop: 12, color: "var(--color-success, #4caf50)", fontSize: 13 }}>
              {status}
            </p>
          )}
          {error && (
            <p style={{ marginTop: 12, color: "var(--color-error, #f44336)", fontSize: 13 }}>
              {error}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
