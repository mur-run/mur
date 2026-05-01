// Step 4 — first memory. The highest-impact field in the wizard.
//
// This text is the seed for `{{FIRST_MEMORY}}` substitution in the
// morning_greeting template (M2.2.3) and is one of the only durable
// per-relationship facts the agent carries. We hard-cap at 200 chars
// (matches the FirstMemory schema in mur-common::companion) and show a
// live remaining-character counter. Empty is allowed — submit sends
// `null` so `init::run` records "no first_memory" rather than an empty
// string.

const MAX = 200;

interface Props {
  value: string;
  onChange: (next: string) => void;
  onBack: () => void;
  onNext: () => void;
}

export function Step4FirstMemory({ value, onChange, onBack, onNext }: Props) {
  const remaining = MAX - value.length;
  const overLimit = remaining < 0;

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className="text-lg font-medium mb-1"
          style={{ color: "var(--color-fg)" }}
        >
          Share one fact about you, in a sentence
        </h2>
        <p className="text-sm opacity-80">
          Anything memorable. I'll bring it up later. This is the
          highest-impact thing in this wizard — but it's also OK to skip.
        </p>
      </div>

      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        maxLength={MAX}
        rows={4}
        placeholder="e.g. I'm learning to bake sourdough on weekends."
        className="rounded-md border px-3 py-2 text-sm outline-none resize-none"
        style={{
          borderColor: "var(--color-border)",
          background: "var(--color-bg)",
          color: "var(--color-fg)",
        }}
      />

      <div
        className="text-xs"
        style={{
          color: overLimit
            ? "var(--color-error, #b91c1c)"
            : "var(--color-fg)",
          opacity: overLimit ? 1 : 0.6,
        }}
      >
        {remaining} characters remaining
      </div>

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
          onClick={onNext}
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
