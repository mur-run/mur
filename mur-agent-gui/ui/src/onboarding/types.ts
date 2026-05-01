// Mirrors the M2.4 Tauri command surface in
// mur-agent-gui/src-tauri/src/commands.rs (companion_onboarding_*).
//
// `Relationship` and `ProactiveTier` are the same string literals the
// CLI's `--answers` YAML accepts (see mur-core/src/cmd/agent_companion/
// init.rs), so the wizard payload round-trips through `init::run`
// without re-mapping.

export type Relationship =
  | "friend"
  | "coach"
  | "accountability_buddy"
  | "mentor";

export type ProactiveTier =
  | "off"
  | "warm_only"
  | "warm_and_behavior"
  | "all";

/// Returned by `companion_onboarding_status`. `completed_at` is the
/// RFC 3339 timestamp the wizard wrote on submit/skip; the wizard reads
/// it on launch to decide whether to show itself or step aside.
export interface OnboardingStatus {
  completed_at: string | null;
  agent_display_name: string | null;
  first_memory_text: string | null;
  /// Lower-case `ProactiveTier` discriminant. Sourced from
  /// `ProactiveTier::from_config`, so a profile that's never been
  /// onboarded reports `"off"` rather than a missing/empty string.
  proactive_tier: string;
}

/// Payload accepted by `companion_onboarding_submit`.
///
/// `re_init` defaults to `false` server-side (`#[serde(default)]`), but
/// TypeScript requires it explicitly — pass `false` for the first-launch
/// wizard and `true` only from a future "Edit" entrypoint.
export interface OnboardingSubmit {
  agent_display_name: string;
  locale: string;
  name_for_user: string;
  relationship: Relationship;
  first_memory: string | null;
  proactive_tier: ProactiveTier;
  re_init: boolean;
}
