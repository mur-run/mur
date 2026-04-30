// Step 1 of the onboarding wizard — display name.
//
// The user names *the agent*, not themselves. We disable Next until the
// trimmed input is non-empty so an accidental Enter through the wizard
// can't produce an unnamed agent. A "Skip onboarding" secondary button
// drops out of the wizard entirely (the shell wires this to
// skipOnboarding(), which lands on the WarmOnly default tier).

interface Props {
  value: string;
  onChange: (next: string) => void;
  onNext: () => void;
  onSkip: () => void;
}

export function Step1AgentName({ value, onChange, onNext, onSkip }: Props) {
  const canAdvance = value.trim().length > 0;

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className="text-lg font-medium mb-1"
          style={{ color: "var(--color-fg)" }}
        >
          What should this agent be called?
        </h2>
        <p className="text-sm opacity-80">
          A display name — something short and human. You can change it
          later.
        </p>
      </div>

      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && canAdvance) onNext();
        }}
        autoFocus
        placeholder="e.g. Iris, Coach, Pebble"
        className="rounded-md border px-3 py-2 text-sm outline-none"
        style={{
          borderColor: "var(--color-border)",
          background: "var(--color-bg)",
          color: "var(--color-fg)",
        }}
      />

      <div className="flex items-center justify-between mt-2">
        <button
          type="button"
          onClick={onSkip}
          className="text-sm underline opacity-70 hover:opacity-100"
          style={{ color: "var(--color-fg)" }}
        >
          Skip onboarding
        </button>
        <button
          type="button"
          onClick={onNext}
          disabled={!canAdvance}
          className="px-4 py-2 text-sm rounded font-medium"
          style={{
            background: canAdvance
              ? "var(--color-accent)"
              : "var(--color-bg-secondary)",
            color: canAdvance
              ? "var(--color-accent-fg)"
              : "var(--color-fg)",
            opacity: canAdvance ? 1 : 0.5,
            cursor: canAdvance ? "pointer" : "not-allowed",
          }}
        >
          Next
        </button>
      </div>
    </div>
  );
}
