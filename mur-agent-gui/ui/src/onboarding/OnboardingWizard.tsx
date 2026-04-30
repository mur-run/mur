// 5-step modal wizard shell.
//
// Owns the local state for every wizard answer plus the current step
// (1-5), renders one step component at a time, and is the single place
// that calls `submitOnboarding` / `skipOnboarding`. App.tsx (M2.6
// territory) hides the wizard via `onComplete` once the round-trip
// succeeds — both the Skip path and the Finish path.
//
// `re_init` is hard-coded to `false`. A future "Edit" entrypoint
// against an already-onboarded agent will need a different shell that
// passes `true`; we explicitly do NOT support that here so a misroute
// can never silently overwrite a user's first_memory.

import { useState } from "react";
import { skipOnboarding, submitOnboarding } from "./api";
import type { ProactiveTier, Relationship } from "./types";
import { Step1AgentName } from "./Step1AgentName";
import { Step2VoicePick } from "./Step2VoicePick";
import { Step3Relationship } from "./Step3Relationship";
import { Step4FirstMemory } from "./Step4FirstMemory";
import { Step5ProactiveTiers } from "./Step5ProactiveTiers";

const LOCALE_DEFAULT =
  typeof navigator !== "undefined" && navigator.language
    ? navigator.language
    : "en-US";

interface Props {
  agent: string;
  /// Called once the wizard is finished (skip OR final submit) so the
  /// caller can hide it. The caller is also responsible for any
  /// follow-up state refresh.
  onComplete: () => void;
  /// Called from Step 2 when the user picks "Enable voice (opens
  /// picker)". The wizard advances to Step 3 immediately so the user
  /// isn't blocked while they configure voice in another tab.
  onOpenVoiceSettings: () => void;
}

type StepIdx = 1 | 2 | 3 | 4 | 5;

export function OnboardingWizard({
  agent,
  onComplete,
  onOpenVoiceSettings,
}: Props) {
  const [step, setStep] = useState<StepIdx>(1);
  const [agentDisplay, setAgentDisplay] = useState("");
  const [nameForUser, setNameForUser] = useState("");
  const [rel, setRel] = useState<Relationship>("friend");
  const [memory, setMemory] = useState("");
  const [tier, setTier] = useState<ProactiveTier>("warm_only");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const goto = (next: StepIdx) => {
    setErr(null);
    setStep(next);
  };

  const skipAll = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await skipOnboarding(agent);
      onComplete();
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  const finish = async (chosenTier: ProactiveTier) => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await submitOnboarding(agent, {
        agent_display_name: agentDisplay,
        locale: LOCALE_DEFAULT,
        name_for_user: nameForUser,
        relationship: rel,
        first_memory: memory.trim().length === 0 ? null : memory,
        proactive_tier: chosenTier,
        // Hard-coded false — see file header. The future "Edit"
        // entrypoint will need its own shell.
        re_init: false,
      });
      onComplete();
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: "rgba(0, 0, 0, 0.5)" }}
      role="dialog"
      aria-modal="true"
      aria-label="Companion onboarding"
    >
      <div
        className="rounded-lg border p-5 shadow-lg"
        style={{
          width: "480px",
          maxWidth: "92vw",
          maxHeight: "90vh",
          overflowY: "auto",
          borderColor: "var(--color-border)",
          background: "var(--color-bg-secondary)",
        }}
      >
        <div className="flex items-center justify-between mb-4">
          <span className="text-xs uppercase tracking-wider opacity-60">
            Welcome · agent: {agent}
          </span>
          <span className="text-xs opacity-60">Step {step} of 5</span>
        </div>

        {step === 1 && (
          <Step1AgentName
            value={agentDisplay}
            onChange={setAgentDisplay}
            onNext={() => goto(2)}
            onSkip={skipAll}
          />
        )}

        {step === 2 && (
          <Step2VoicePick
            onBack={() => goto(1)}
            onNext={() => goto(3)}
            onEnable={onOpenVoiceSettings}
          />
        )}

        {step === 3 && (
          <Step3Relationship
            locale={LOCALE_DEFAULT}
            nameForUser={nameForUser}
            relationship={rel}
            onNameChange={setNameForUser}
            onRelationshipChange={setRel}
            onBack={() => goto(2)}
            onNext={() => goto(4)}
          />
        )}

        {step === 4 && (
          <Step4FirstMemory
            value={memory}
            onChange={setMemory}
            onBack={() => goto(3)}
            onNext={() => goto(5)}
          />
        )}

        {step === 5 && (
          <Step5ProactiveTiers
            value={tier}
            onChange={setTier}
            onBack={() => goto(4)}
            onSubmit={finish}
            busy={busy}
          />
        )}

        {err && (
          <div
            className="mt-4 text-sm"
            style={{ color: "var(--color-error, #b91c1c)" }}
          >
            {err}
          </div>
        )}
      </div>
    </div>
  );
}
