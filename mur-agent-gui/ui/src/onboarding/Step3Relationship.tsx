// Step 3 — locale + name_for_user + relationship picker.
//
// The locale stays out of sight (the shell defaults from
// navigator.language); this step's two interactive concerns are:
//   * the name the agent should call the user
//   * the relationship card (4 options, each with an example greeting
//     matched 1:1 against the CLI's `example_greeting` helper in
//     mur-core/src/cmd/agent_companion/init.rs §273-291)
//
// Selection state is rendered with the `--color-accent` border so the
// active card is obvious in any theme.

import type { Relationship } from "./types";

interface Props {
  locale: string;
  nameForUser: string;
  relationship: Relationship;
  onNameChange: (next: string) => void;
  onRelationshipChange: (next: Relationship) => void;
  onBack: () => void;
  onNext: () => void;
}

interface Card {
  value: Relationship;
  title: string;
  exampleEn: string;
  exampleZh: string;
}

const CARDS: Card[] = [
  {
    value: "friend",
    title: "Friend",
    exampleEn: "\"Hey — how's it going?\"",
    exampleZh: "「嗨，今天好嗎？」",
  },
  {
    value: "coach",
    title: "Coach",
    exampleEn: "\"Hey. What's the goal?\"",
    exampleZh: "「嗨，目標是什麼？」",
  },
  {
    value: "accountability_buddy",
    title: "Accountability buddy",
    exampleEn: "\"Hi, what's on your plate today?\"",
    exampleZh: "「嗨，今天有什麼想做的嗎？」",
  },
  {
    value: "mentor",
    title: "Mentor",
    exampleEn: "\"Hi. What's been on your mind?\"",
    exampleZh: "「嗨，最近在思考什麼？」",
  },
];

function exampleFor(card: Card, locale: string): string {
  if (locale.startsWith("zh")) return card.exampleZh;
  // Match the CLI: zh-* prints zh-TW phrasing; en-* / fallback prints
  // English (init.rs §273-291 prefers no-greeting for other locales,
  // but here we always show *something* so the cards aren't blank).
  return card.exampleEn;
}

export function Step3Relationship({
  locale,
  nameForUser,
  relationship,
  onNameChange,
  onRelationshipChange,
  onBack,
  onNext,
}: Props) {
  const canAdvance = nameForUser.trim().length > 0;

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className="text-lg font-medium mb-1"
          style={{ color: "var(--color-fg)" }}
        >
          How should this agent relate to you?
        </h2>
        <p className="text-sm opacity-80">
          Pick what fits today — you can change it later.
        </p>
      </div>

      <label className="flex flex-col gap-1">
        <span className="text-sm opacity-80">What should I call you?</span>
        <input
          type="text"
          value={nameForUser}
          onChange={(e) => onNameChange(e.target.value)}
          placeholder="Your name or a nickname"
          className="rounded-md border px-3 py-2 text-sm outline-none"
          style={{
            borderColor: "var(--color-border)",
            background: "var(--color-bg)",
            color: "var(--color-fg)",
          }}
        />
      </label>

      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        {CARDS.map((c) => {
          const active = c.value === relationship;
          return (
            <button
              key={c.value}
              type="button"
              role="radio"
              aria-checked={active}
              onClick={() => onRelationshipChange(c.value)}
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
                {c.title}
              </div>
              <div className="text-xs opacity-70 mt-1">
                {exampleFor(c, locale)}
              </div>
            </button>
          );
        })}
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
