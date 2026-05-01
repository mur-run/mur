// Thin wrappers around the M2.4 onboarding Tauri commands. Mirrors the
// `companion_onboarding_*` surface in
// mur-agent-gui/src-tauri/src/commands.rs.
//
// Helper modules wrap each command 1:1 so the wizard components stay
// agnostic of `invoke()` and the underlying error shape — an
// invoke-failure surfaces as a thrown string, which the wizard catches
// and renders inline.

import { invoke } from "@tauri-apps/api/core";
import type { OnboardingStatus, OnboardingSubmit } from "./types";

export const getOnboardingStatus = (agent: string) =>
  invoke<OnboardingStatus>("companion_onboarding_status", { agent });

export const submitOnboarding = (agent: string, payload: OnboardingSubmit) =>
  invoke<void>("companion_onboarding_submit", { agent, payload });

export const skipOnboarding = (agent: string) =>
  invoke<void>("companion_onboarding_skip", { agent });
