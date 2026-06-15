import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../../i18n";
import type { SpecDraftDto } from "./SpecGenerating";

interface Props {
  draft: SpecDraftDto;
  /** Called with the created agent name after approve succeeds. */
  onCreated: (agentName: string) => void;
}

export function SpecReview({ draft, onCreated }: Props) {
  const { t } = useT();

  // Editable local copies
  const [prompt, setPrompt] = useState(draft.prompt_markdown);
  const [skillYamls, setSkillYamls] = useState<Record<string, string>>(
    () =>
      Object.fromEntries(draft.skills.map((s) => [s.name, s.yaml])),
  );
  const [approving, setApproving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function handleSkillChange(name: string, value: string) {
    setSkillYamls((prev) => ({ ...prev, [name]: value }));
  }

  async function handleApprove() {
    setApproving(true);
    setError(null);
    const edited: SpecDraftDto = {
      ...draft,
      prompt_markdown: prompt,
      skills: draft.skills.map((s) => ({
        ...s,
        yaml: skillYamls[s.name] ?? s.yaml,
      })),
    };
    try {
      const name = await invoke<string>("wizard_spec_approve", {
        edited,
        runEval: true,
      });
      onCreated(name);
    } catch (e) {
      setError(String(e));
      setApproving(false);
    }
  }

  // Render entitlements as a readable list.
  // `entitlements` is a freeform JSON value from the backend.
  function renderEntitlements() {
    const ent = draft.entitlements;
    if (!ent || (typeof ent === "object" && Object.keys(ent).length === 0)) {
      return <p className="wz-hint">{t("wizard.review.entitlements.none")}</p>;
    }
    if (typeof ent === "object" && !Array.isArray(ent)) {
      const entries = Object.entries(ent as Record<string, unknown>);
      return (
        <ul className="wz-review-ent-list">
          {entries.map(([key, val]) => (
            <li key={key} className="wz-review-ent-item">
              <span className="wz-review-ent-key">{key}:</span>{" "}
              <span className="wz-review-ent-val">
                {Array.isArray(val) ? (val as unknown[]).join(", ") : String(val)}
              </span>
            </li>
          ))}
        </ul>
      );
    }
    // Fallback: show raw JSON
    return (
      <pre className="wz-review-ent-raw">
        {JSON.stringify(ent, null, 2)}
      </pre>
    );
  }

  return (
    <div className="wz-step wz-review">
      <h2>{t("wizard.review.title")}</h2>
      <p className="wz-hint">{t("wizard.review.hint")}</p>

      {/* Agent info */}
      <div className="wz-review-meta">
        <span className="wz-review-meta-name">{draft.display_name}</span>
        <span className="wz-review-meta-role">{draft.charter}</span>
      </div>

      {/* Editable system prompt */}
      <section className="wz-review-section">
        <h3>{t("wizard.review.prompt")}</h3>
        <textarea
          className="wz-review-textarea wz-review-textarea--prompt"
          value={prompt}
          rows={10}
          onChange={(e) => setPrompt(e.target.value)}
          spellCheck={false}
        />
      </section>

      {/* Per-skill YAML editors */}
      {draft.skills.length > 0 && (
        <section className="wz-review-section">
          <h3>{t("wizard.review.skills")}</h3>
          {draft.skills.map((skill) => (
            <div key={skill.name} className="wz-review-skill">
              <label className="wz-review-skill-name">{skill.name}</label>
              <textarea
                className="wz-review-textarea wz-review-textarea--skill"
                value={skillYamls[skill.name] ?? skill.yaml}
                rows={6}
                onChange={(e) => handleSkillChange(skill.name, e.target.value)}
                spellCheck={false}
              />
            </div>
          ))}
        </section>
      )}

      {/* Entitlement summary */}
      <section className="wz-review-section">
        <h3>{t("wizard.review.entitlements")}</h3>
        {renderEntitlements()}
      </section>

      {error && (
        <p className="wz-error">{error}</p>
      )}

      <div className="wz-role-actions">
        <button
          className="btn btn--primary"
          disabled={approving}
          onClick={handleApprove}
        >
          {approving
            ? t("wizard.review.approving")
            : t("wizard.review.approve")}
        </button>
      </div>
    </div>
  );
}
