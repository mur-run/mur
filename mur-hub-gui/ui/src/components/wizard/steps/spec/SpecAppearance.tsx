import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../../i18n";
import { BUILTIN_PRESETS, type AgentDetail, type DetailPatch } from "../../../../types";
import { PetFace } from "../../../PetFace";

interface Props {
  /** The specialist agent that was just created (it already exists on disk). */
  agentName: string;
  onDone: (name: string) => void;
}

/**
 * "Both" flow, final step: give the freshly-created specialist a companion pet
 * look. Picking a style writes `style_preset` onto the existing agent (offline
 * PetFace shows immediately); rendering AI expressions is optional and can also
 * be done later in the detail panel's Style tab. Reuses the same backend
 * commands as that tab — no new plumbing.
 */
export function SpecAppearance({ agentName, onDone }: Props) {
  const { t } = useT();
  const [selected, setSelected] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [rendering, setRendering] = useState(false);
  const [rendered, setRendered] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function pick(id: string) {
    setSelected(id);
    setSaving(true);
    setError(null);
    setRendered(false);
    try {
      await invoke<AgentDetail>("update_agent_detail", {
        name: agentName,
        patch: { style_preset: id } as DetailPatch,
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  // Poll render status while a render is in flight (mirrors the detail Style tab).
  useEffect(() => {
    if (!rendering) return;
    let cancelled = false;
    const started = Date.now();
    const timer = setInterval(async () => {
      try {
        const fresh = await invoke<AgentDetail>("get_agent_detail", {
          name: agentName,
        });
        if (cancelled) return;
        if (fresh.render_status.status === "ready") {
          setRendering(false);
          setRendered(true);
        } else if (fresh.render_status.status === "failed") {
          setRendering(false);
          setError(t("wizard.appearance.renderFailed"));
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
  }, [rendering, agentName, t]);

  async function renderNow() {
    if (!selected) return;
    setError(null);
    setRendering(true);
    try {
      await invoke("render_agent_expressions", { name: agentName });
    } catch (e) {
      setError(String(e));
      setRendering(false);
    }
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.appearance.title")}</h2>
      <p className="wz-hint">{t("wizard.appearance.hint")}</p>

      <div className="wz-preset-grid">
        {BUILTIN_PRESETS.map((p) => (
          <button
            key={p.id}
            className={`wz-preset-card${selected === p.id ? " selected" : ""}`}
            onClick={() => pick(p.id)}
            disabled={saving || rendering}
          >
            <PetFace
              presetId={p.id}
              family={p.family}
              expression="smile"
              size={56}
              animate={false}
            />
            <span className="wz-preset-name">{p.display_name}</span>
            <span className="wz-preset-desc">{p.description}</span>
          </button>
        ))}
      </div>

      {rendered && (
        <p className="wz-hint" style={{ color: "var(--color-success, #4caf50)" }}>
          {t("wizard.appearance.rendered")}
        </p>
      )}
      {error && <p className="wz-error">{error}</p>}

      <div
        style={{
          display: "flex",
          gap: 8,
          marginTop: 16,
          justifyContent: "flex-end",
        }}
      >
        {selected && !rendered && (
          <button className="btn" onClick={renderNow} disabled={rendering}>
            {rendering
              ? t("wizard.appearance.rendering")
              : t("wizard.appearance.render")}
          </button>
        )}
        <button
          className="btn btn--primary"
          onClick={() => onDone(agentName)}
          disabled={rendering}
        >
          {selected ? t("wizard.appearance.done") : t("wizard.appearance.skip")}
        </button>
      </div>
    </div>
  );
}
