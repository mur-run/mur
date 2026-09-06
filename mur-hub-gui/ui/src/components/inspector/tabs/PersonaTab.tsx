import { useState } from "react";
import { useMarkDirty } from "../../shell/dirty";
import { invoke } from "@tauri-apps/api/core";
import type { AgentDetail, DetailPatch } from "../../../types";
import { useT } from "../../../i18n";

const PERSONA_CATEGORIES = [
  "research", "automation", "monitor", "notify", "commerce", "custom",
];

const TONE_OPTIONS = ["professional", "casual", "friendly", "direct", "playful", "formal"];
const RISK_OPTIONS = ["conservative", "balanced", "bold"];
const VERBOSITY_OPTIONS = ["concise", "balanced", "detailed"];

// Include the agent's current value as an option even when it isn't one of the
// canned choices (older agents / custom vocab), so the <select> shows the real
// value instead of silently snapping to the first option — which misrepresents
// the persona and risks clobbering the value on save.
function withCurrent(options: string[], current: string): string[] {
  return current && !options.includes(current) ? [current, ...options] : options;
}

// Bundled default roles (suggestions only; the field is free-text so users
// can pick one or type their own). Grounded in MetaGPT's software roles +
// generic knowledge-work archetypes. ponytail: a datalist, not a registry.
const ROLE_SUGGESTIONS = [
  "Engineer",
  "Architect",
  "QA",
  "Product Manager",
  "Researcher",
  "Writer",
  "Analyst",
  "Coordinator",
];

export function PersonaTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [role, setRole] = useState(detail.role ?? "");
  const [category, setCategory] = useState(detail.persona_category);
  const [description, setDescription] = useState(detail.persona_description);
  const [tone, setTone] = useState(detail.persona_tone);
  const [risk, setRisk] = useState(detail.persona_risk);
  const [verbosity, setVerbosity] = useState(detail.persona_verbosity);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  async function save() {
    setSaving(true);
    setSaveError(null);
    try {
      const patch: DetailPatch = {
        role: role.trim(),
        persona_category: category,
        persona_description: description,
        persona_tone: tone,
        persona_risk: risk,
        persona_verbosity: verbosity,
      };
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch,
      });
      onSaved(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  const changed =
    role.trim() !== (detail.role ?? "") ||
    category !== detail.persona_category ||
    description !== detail.persona_description ||
    tone !== detail.persona_tone ||
    risk !== detail.persona_risk ||
    verbosity !== detail.persona_verbosity;
  // Unsaved edits block selection / tab changes until saved or discarded (spec §6.5).
  useMarkDirty("persona", changed);

  // Visual meter level for risk / verbosity (0-2 → 1-3 bars on).
  const riskLevel = Math.max(0, RISK_OPTIONS.indexOf(risk));
  const verbosityLevel = Math.max(0, VERBOSITY_OPTIONS.indexOf(verbosity));

  return (
    <div className="tab-form">
      <label className="field-label">{t("detail.role")}</label>
      <input
        className="input"
        list="role-suggestions"
        value={role}
        onChange={(e) => { setRole(e.target.value); }}
        placeholder={t("detail.rolePlaceholder")}
      />
      <datalist id="role-suggestions">
        {ROLE_SUGGESTIONS.map((r) => (
          <option key={r} value={r} />
        ))}
      </datalist>

      <label className="field-label">{t("detail.category")}</label>
      <select
        className="input"
        value={category}
        onChange={(e) => { setCategory(e.target.value); }}
      >
        {PERSONA_CATEGORIES.map((c) => (
          <option key={c} value={c}>{c}</option>
        ))}
      </select>

      <label className="field-label">{t("detail.description")}</label>
      <textarea
        className="input"
        rows={3}
        value={description}
        onChange={(e) => { setDescription(e.target.value); }}
        placeholder={t("detail.descPlaceholder")}
      />

      <label className="field-label">{t("detail.tone")}</label>
      <select
        className="input"
        value={tone}
        onChange={(e) => { setTone(e.target.value); }}
      >
        {withCurrent(TONE_OPTIONS, tone).map((opt) => (
          <option key={opt} value={opt}>{opt}</option>
        ))}
      </select>

      <label className="field-label">{t("detail.risk")}</label>
      <div className="meter">
        {[0, 1, 2].map((i) => (
          <i key={i} className={i <= riskLevel ? "is-on" : ""} />
        ))}
      </div>
      <select
        className="input"
        value={risk}
        onChange={(e) => { setRisk(e.target.value); }}
      >
        {withCurrent(RISK_OPTIONS, risk).map((r) => (
          <option key={r} value={r}>{r}</option>
        ))}
      </select>

      <label className="field-label">{t("detail.verbosity")}</label>
      <div className="meter">
        {[0, 1, 2].map((i) => (
          <i key={i} className={i <= verbosityLevel ? "is-on" : ""} />
        ))}
      </div>
      <select
        className="input"
        value={verbosity}
        onChange={(e) => { setVerbosity(e.target.value); }}
      >
        {withCurrent(VERBOSITY_OPTIONS, verbosity).map((v) => (
          <option key={v} value={v}>{v}</option>
        ))}
      </select>

      <button
        className="btn btn--primary btn--sm save-btn"
        disabled={!changed || saving}
        onClick={save}
      >
        {saving ? t("detail.saving") : saved ? t("detail.saved") : t("detail.save")}
      </button>
      {changed && !saving && <span className="field-muted">{t("detail.unsaved")}</span>}
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}
