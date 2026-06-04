import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../i18n";

interface Props {
  isOpen: boolean;
  onClose: () => void;
}

type Mode = "file" | "url";

export function PresetImportModal({ isOpen, onClose }: Props) {
  const { t } = useT();
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
      setStatus(t("modal.preset.imported", { id }));
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
      setStatus(t("modal.preset.imported", { id }));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="modal__overlay" onClick={handleClose}>
      <div
        className="modal"
        style={{ width: 440 }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__header">
          <h2 className="modal__title">{t("modal.preset.title")}</h2>
          <button className="modal__close" onClick={handleClose}>
            ×
          </button>
        </div>

        <div className="modal__body">
          <div className="tab-row" style={{ display: "flex", gap: 8, marginBottom: 16 }}>
            <button
              className={`toolbar-btn${mode === "file" ? " active" : ""}`}
              onClick={() => setMode("file")}
            >
              {t("modal.preset.fromFile")}
            </button>
            <button
              className={`toolbar-btn${mode === "url" ? " active" : ""}`}
              onClick={() => setMode("url")}
            >
              {t("modal.preset.fromUrl")}
            </button>
          </div>

          {mode === "file" ? (
            <div>
              <p style={{ marginBottom: 12, color: "var(--text-secondary, #888)", fontSize: 13 }}>
                {t("modal.preset.file.body")}
              </p>
              <button className="btn btn--primary" onClick={importFile} disabled={loading}>
                {loading ? t("modal.preset.file.importing") : t("modal.preset.file.choose")}
              </button>
            </div>
          ) : (
            <div>
              <p style={{ marginBottom: 8, color: "var(--text-secondary, #888)", fontSize: 13 }}>
                {t("modal.preset.url.body")}
              </p>
              <input
                className="input"
                type="url"
                placeholder={t("modal.preset.url.placeholder")}
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && importUrl()}
                style={{ width: "100%", marginBottom: 8 }}
              />
              <button
                className="btn btn--primary"
                onClick={importUrl}
                disabled={loading || !url.trim()}
              >
                {loading ? t("modal.preset.url.fetching") : t("modal.preset.url.import")}
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
