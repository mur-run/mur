// Step 2 — voice consent. Mirrors the CLI's two-option Select in
// `run_wizard()` (mur-core/src/cmd/agent_companion/init.rs §177-194):
// either skip and enable later from Settings, or open the picker now.
//
// Like the CLI, this step records *intent* only — the actual voice
// download lives in the existing Voice tab + VoiceEnablePanel. When the
// user picks "Enable", the shell hands off to App.tsx via
// `onOpenVoiceSettings` and then advances to step 3 immediately so the
// wizard doesn't block the picker.

import { useState } from "react";

type Choice = "skip" | "enable";

interface Props {
  onBack: () => void;
  onNext: () => void;
  onEnable: () => void;
}

export function Step2VoicePick({ onBack, onNext, onEnable }: Props) {
  const [choice, setChoice] = useState<Choice>("skip");

  const advance = () => {
    if (choice === "enable") {
      onEnable();
    }
    onNext();
  };

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className="text-lg font-medium mb-1"
          style={{ color: "var(--color-fg)" }}
        >
          Voice
        </h2>
        <p className="text-sm opacity-80">
          Voice setup is opt-in and downloads ~190&nbsp;MB on first use.
          We never send audio off your machine.
        </p>
      </div>

      <select
        value={choice}
        onChange={(e) => setChoice(e.target.value as Choice)}
        className="rounded-md border px-3 py-2 text-sm outline-none"
        style={{
          borderColor: "var(--color-border)",
          background: "var(--color-bg)",
          color: "var(--color-fg)",
        }}
      >
        <option value="skip">Skip — enable voice later in Settings</option>
        <option value="enable">Enable voice (opens picker)</option>
      </select>

      <div className="flex items-center justify-between mt-2">
        <button
          type="button"
          onClick={onBack}
          className="text-sm underline opacity-70 hover:opacity-100"
          style={{ color: "var(--color-fg)" }}
        >
          Back
        </button>
        <button
          type="button"
          onClick={advance}
          className="px-4 py-2 text-sm rounded font-medium"
          style={{
            background: "var(--color-accent)",
            color: "var(--color-accent-fg)",
          }}
        >
          Next
        </button>
      </div>
    </div>
  );
}
