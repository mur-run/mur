import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { BUILTIN_PRESETS, type AgentDetail, type DetailPatch } from "../../../types";
import { useT } from "../../../i18n";
import { PetFace } from "../../PetFace";

export function StyleTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [selected, setSelected] = useState(detail.style_preset);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(detail.render_status.status === "rendering");

  // Poll for render completion while a render is in flight, refreshing the
  // panel so the status text/progress and preset thumbnails stay current.
  useEffect(() => {
    if (!rendering) return;
    let cancelled = false;
    const started = Date.now();
    const timer = setInterval(async () => {
      try {
        const fresh = await invoke<AgentDetail>("get_agent_detail", {
          name: detail.agent_name,
        });
        if (cancelled) return;
        onSaved(fresh);
        if (fresh.render_status.status === "ready" || fresh.render_status.status === "failed") {
          setRendering(false);
        }
      } catch {
        // transient; keep polling
      }
      if (Date.now() - started > 120_000) setRendering(false); // safety timeout
    }, 1500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [rendering, detail.agent_name, onSaved]);

  // Whether real AI art exists. When false the pet shows the built-in vector
  // mascot, and we surface a gentle "connect an image model" hint.
  const [hasAiArt, setHasAiArt] = useState(true);
  useEffect(() => {
    invoke<{ has_ai_art: boolean }>("pet_get_appearance", { agentName: detail.agent_name })
      .then((a) => setHasAiArt(a.has_ai_art))
      .catch(() => setHasAiArt(false));
    // depend on the status STRING, not the render_status object (which gets a new
    // identity every 1.5s poll tick → ~80 redundant IPC reads during a render).
  }, [detail.agent_name, detail.render_status.status]);

  async function triggerRender() {
    setSaveError(null);
    setRendering(true);
    try {
      await invoke("render_agent_expressions", { name: detail.agent_name });
    } catch (e) {
      setSaveError(String(e));
      setRendering(false);
    }
  }

  async function pickPreset(id: string) {
    setSelected(id);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { style_preset: id } as DetailPatch,
      });
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  function statusText(): string {
    switch (detail.render_status.status) {
      case "pending": return t("detail.notRendered");
      case "rendering": return t("detail.rendering", {
        done: detail.render_status.done,
        total: detail.render_status.total,
      });
      case "ready": return t("detail.renderReady");
      case "failed": return t("detail.renderFailed", { reason: detail.render_status.reason });
    }
  }

  const current = BUILTIN_PRESETS.find((p) => p.id === selected);

  return (
    <div className="tab-form">
      <label className="field-label">{t("detail.currentStyle")}</label>
      <div className="current-style">
        <PetFace
          presetId={selected}
          family={current?.family ?? "chibi"}
          expression="smile"
          size={44}
          animate={false}
        />
        <p className="field-value">
          {current?.display_name ?? selected}
          {" "}
          <span className="field-muted">
            ({current?.family ?? t("detail.unknown")})
          </span>
        </p>
      </div>

      <label className="field-label">{t("detail.renderStatus")}</label>
      <p className="field-muted" style={{ fontSize: 12 }}>{statusText()}</p>
      {detail.render_status.status === "rendering" && (
        <div className="progress-bar" style={{ marginTop: 6 }}>
          <div
            className="progress-fill"
            style={{
              width: `${(detail.render_status.done / detail.render_status.total) * 100}%`,
            }}
          />
        </div>
      )}
      <button
        className={
          detail.render_status.status === "pending"
            ? "btn btn--sm btn--primary"
            : "toolbar-btn"
        }
        style={{ marginTop: 8 }}
        onClick={triggerRender}
        disabled={rendering || detail.render_status.status === "rendering"}
        title={t("detail.renderTooltip")}
      >
        {rendering || detail.render_status.status === "rendering"
          ? t("detail.renderBtnRendering")
          : detail.render_status.status === "ready"
            ? t("detail.renderBtnRerender")
            : t("detail.renderBtnRender")}
      </button>

      {!hasAiArt && (
        <p className="field-muted detail-vector-hint" style={{ fontSize: 12, marginTop: 8 }}>
          {t("detail.vectorHint")}
        </p>
      )}

      <label className="field-label" style={{ marginTop: 16 }}>{t("detail.presetGallery")}</label>
      <div className="style-gallery">
        {BUILTIN_PRESETS.map((p) => (
          <button
            key={p.id}
            className={`style-thumb${selected === p.id ? " is-selected" : ""}`}
            onClick={() => pickPreset(p.id)}
            disabled={saving}
            title={p.description}
          >
            <PetFace
              presetId={p.id}
              family={p.family}
              expression="smile"
              size={48}
              animate={false}
            />
            <div className="style-thumb__label">{p.display_name}</div>
            <div className="style-thumb__family">{p.family}</div>
          </button>
        ))}
      </div>
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}
