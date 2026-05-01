// Step 5 — three-layer proactive toggle.
//
// Mirrors the CLI's terminal step (init.rs §236-252): "Warm voice only"
// is the recommended default and stays that way unless the user picks
// otherwise. The three options map 1:1 to the `ProactiveTier` variants
// the M2.4 submit handler accepts:
//
//   warm_only         → companion enabled, rhythm dormant, proactive off
//   warm_and_behavior → companion + rhythm collection, proactive off
//   all               → companion + rhythm + proactive sends enabled
//
// The "off" variant isn't offered here because the wizard exists to
// turn the companion *on* — users who don't want any of this hit Skip
// in step 1 (which lands on warm_only via skipOnboarding()) or keep
// proactive off via `mur agent companion proactive disable` later.

import type { ProactiveTier } from "./types";

interface Props {
  value: ProactiveTier;
  onChange: (next: ProactiveTier) => void;
  onBack: () => void;
  onSubmit: (tier: ProactiveTier) => void;
  busy: boolean;
}

interface Option {
  value: ProactiveTier;
  title: string;
  desc: string;
}

const OPTIONS: Option[] = [
  {
    value: "warm_only",
    title: "Warm voice only (recommended)",
    desc: "I'll respond in a more human tone, but I'll never message you first.",
  },
  {
    value: "warm_and_behavior",
    title: "Warm + behavior collection",
    desc: "I'll quietly learn your rhythm so a future opt-in is well-timed. Still no proactive sends.",
  },
  {
    value: "all",
    title: "All — including proactive check-ins",
    desc: "I may send the occasional warm check-in once I've earned permission. You can pause or turn this off any time.",
  },
];

export function Step5ProactiveTiers({
  value,
  onChange,
  onBack,
  onSubmit,
  busy,
}: Props) {
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className="text-lg font-medium mb-1"
          style={{ color: "var(--color-fg)" }}
        >
          How should I behave?
        </h2>
        <p className="text-sm opacity-80">
          You can change this any time from the Voice tab or with{" "}
          <code>mur agent companion proactive</code>.
        </p>
      </div>

      <div className="flex flex-col gap-3">
        {OPTIONS.map((opt) => {
          const active = opt.value === value;
          return (
            <button
              key={opt.value}
              type="button"
              role="radio"
              aria-checked={active}
              onClick={() => onChange(opt.value)}
              className="rounded-md border p-3 text-left transition-colors"
              style={{
                borderColor: active
                  ? "var(--color-accent)"
                  : "var(--color-border)",
                background: active
                  ? "var(--color-bg-secondary)"
                  : "var(--color-bg)",
              }}
            >
              <div
                className="font-medium"
                style={{ color: "var(--color-fg)" }}
              >
                {opt.title}
              </div>
              <div className="text-xs opacity-70 mt-1">{opt.desc}</div>
            </button>
          );
        })}
      </div>

      <div className="flex items-center justify-between mt-2">
        <button
          type="button"
          onClick={onBack}
          disabled={busy}
          className="text-sm underline opacity-70 hover:opacity-100"
          style={{ color: "var(--color-fg)" }}
        >
          Back
        </button>
        <button
          type="button"
          onClick={() => onSubmit(value)}
          disabled={busy}
          className="px-4 py-2 text-sm rounded font-medium"
          style={{
            background: "var(--color-accent)",
            color: "var(--color-accent-fg)",
            opacity: busy ? 0.6 : 1,
            cursor: busy ? "wait" : "pointer",
          }}
        >
          {busy ? "Finishing…" : "Finish"}
        </button>
      </div>
    </div>
  );
}
