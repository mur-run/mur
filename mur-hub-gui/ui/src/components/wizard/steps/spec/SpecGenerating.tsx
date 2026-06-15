import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../../../i18n";

/** Progress event payload from wizard-spec-progress. */
interface SpecProgress {
  stage: string;
  message: string;
}

/** Full draft DTO returned by wizard_spec_generate (snake_case from serde). */
export interface SkillDto {
  name: string;
  yaml: string;
}

export interface SpecDraftDto {
  role_id: string;
  display_name: string;
  charter: string;
  risk: string;
  skills: SkillDto[];
  prompt_markdown: string;
  model_ref: string;
  entitlements: unknown;
}

interface Props {
  roleId: string;
  noLlm: boolean;
  /** Called when generation completes with the draft DTO. */
  onDraft: (draft: SpecDraftDto) => void;
}

export function SpecGenerating({ roleId, noLlm, onDraft }: Props) {
  const { t } = useT();
  const [lines, setLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Track whether we've already fired the invoke to avoid StrictMode double-calls.
  const started = useRef(false);

  useEffect(() => {
    // Subscribe to progress events.
    const unsubProgress = listen<SpecProgress>("wizard-spec-progress", (ev) => {
      const msg = ev.payload.message;
      setLines((prev) => [...prev, msg]);
    });

    // Kick off generation only once.
    if (!started.current) {
      started.current = true;
      invoke<SpecDraftDto>("wizard_spec_generate", {
        roleId,
        noLlm,
      })
        .then((draft) => onDraft(draft))
        .catch((e) => setError(String(e)));
    }

    return () => {
      unsubProgress.then((f) => f());
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="wz-step">
      <h2>{t("wizard.generating.title")}</h2>
      <p className="wz-hint">{t("wizard.generating.hint")}</p>

      {!error && (
        <div className="wz-spec-log">
          {lines.length === 0 && (
            <p className="wz-progress-text">{t("wizard.generating.starting")}</p>
          )}
          {lines.map((line, i) => (
            <p key={i} className="wz-spec-log-line">
              {line}
            </p>
          ))}
        </div>
      )}

      {error && (
        <div className="wz-render-error">
          <p className="wz-error">{t("wizard.generating.error")}</p>
          <p className="wz-error-detail">{error}</p>
        </div>
      )}
    </div>
  );
}
