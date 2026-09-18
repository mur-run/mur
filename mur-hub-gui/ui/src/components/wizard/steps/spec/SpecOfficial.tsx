import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../../i18n";
import type { TranslationKey } from "../../../../i18n/types";

type ModelPolicy = "capability-first" | "cost-first" | "privacy-first";

interface ModelRequirements {
  chat: boolean;
  tools: boolean;
  minimum_context_window?: number | null;
}

export interface CatalogItemView {
  id: string;
  tier: string;
  version: string;
  description: string;
  agent_name: string | null;
  model_requirements: ModelRequirements | null;
  min_mur_version: string | null;
  compatible: boolean;
  compatibility_error: string | null;
}

type OfficialSelectionDecision =
  | { kind: "blocked"; error: string }
  | { kind: "policy" }
  | { kind: "install"; selection: "Unchanged" };

export function decideOfficialSelection(item: CatalogItemView): OfficialSelectionDecision {
  if (!item.compatible) {
    return {
      kind: "blocked",
      error: item.compatibility_error ?? "This item requires a newer MUR version.",
    };
  }
  if (item.model_requirements) return { kind: "policy" };
  return { kind: "install", selection: "Unchanged" };
}

type ModelWarning = Record<string, { model_ref: string }>;

export interface ModelSelectionPlan {
  primary: string;
  fallbacks: string[];
  warnings: ModelWarning[];
}

export interface OfficialFlowState {
  stage: "select" | "policy";
  plan: ModelSelectionPlan | null;
  error: string | null;
}

type OfficialFlowEvent = { type: "plan-failed"; error: string };

export function reduceOfficialFlow(
  state: OfficialFlowState,
  event: OfficialFlowEvent,
): OfficialFlowState {
  if (event.type === "plan-failed") {
    return { ...state, stage: "policy", plan: null, error: event.error };
  }
  return state;
}

interface PlanLabels {
  primary: string;
  fallbacks: string;
  warning: (kind: string) => string;
}

export function OfficialPlanPreview({
  plan,
  labels,
}: {
  plan: ModelSelectionPlan;
  labels: PlanLabels;
}) {
  return (
    <div className="wz-role-info" data-testid="official-plan">
      <strong>{labels.primary}: {plan.primary}</strong>
      {plan.fallbacks.length > 0 && (
        <span>{labels.fallbacks}: {plan.fallbacks.join(" → ")}</span>
      )}
      {plan.warnings.map((warning, index) => {
        const [kind, modelRef] = warningParts(warning);
        return <span className="wz-hint" key={`${kind}-${modelRef}-${index}`}>{labels.warning(kind)}: {modelRef}</span>;
      })}
    </div>
  );
}

interface InstallOutcomeView {
  item_id: string;
  agent_name: string | null;
  model_selection: ModelSelectionPlan | null;
}

interface Props {
  onInstalled: (agentName: string | null) => void;
}

export async function previewOfficialModels(id: string, policy: ModelPolicy) {
  return invoke<ModelSelectionPlan>("official_plan_models", { id, policy });
}

export async function installOfficialItem(
  id: string,
  selection: { Automatic: ModelPolicy } | "Unchanged",
) {
  return invoke<InstallOutcomeView>("official_install", {
    id,
    modelSelection: selection,
  });
}

function warningParts(warning: ModelWarning): [string, string] {
  const [kind, detail] = Object.entries(warning)[0] ?? ["Unknown", { model_ref: "" }];
  return [kind, detail.model_ref];
}

export function SpecOfficial({ onInstalled }: Props) {
  const { t } = useT();
  const [items, setItems] = useState<CatalogItemView[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loggedIn, setLoggedIn] = useState(true);
  const [stage, setStage] = useState<"select" | "policy">("select");
  const [policy, setPolicy] = useState<ModelPolicy>("capability-first");
  const [plan, setPlan] = useState<ModelSelectionPlan | null>(null);
  const [planning, setPlanning] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const item = useMemo(() => items.find((candidate) => candidate.id === selected), [items, selected]);

  useEffect(() => {
    invoke<boolean>("official_logged_in").then(setLoggedIn).catch(() => {});
    invoke<CatalogItemView[]>("official_list")
      .then((list) => {
        setItems(list);
        if (list.length > 0) setSelected(list[0].id);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  function chooseItem(id: string) {
    setSelected(id);
    setStage("select");
    setPlan(null);
    setError(null);
  }

  async function preview(nextPolicy: ModelPolicy) {
    if (!item?.compatible || !item.model_requirements) return;
    setPolicy(nextPolicy);
    setPlanning(true);
    setPlan(null);
    setError(null);
    try {
      setPlan(await previewOfficialModels(item.id, nextPolicy));
    } catch (e) {
      // Stay on the policy stage so the user can choose another policy/model setup.
      setError(String(e));
    } finally {
      setPlanning(false);
    }
  }

  async function beginInstall() {
    if (!item) return;
    const decision = decideOfficialSelection(item);
    if (decision.kind === "blocked") {
      setError(decision.error);
      return;
    }
    if (decision.kind === "policy") {
      setStage("policy");
      await preview(policy);
      return;
    }
    await install(decision.selection);
  }

  async function install(selection: { Automatic: ModelPolicy } | "Unchanged") {
    if (!item?.compatible) return;
    setInstalling(true);
    setError(null);
    try {
      const outcome = await installOfficialItem(item.id, selection);
      onInstalled(outcome.agent_name);
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(false);
    }
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.official.title")}</h2>
      <p className="wz-hint">{t("wizard.official.hint")}</p>

      {loading && <p className="wz-progress-text">{t("wizard.loading")}</p>}
      {error && <p className="wz-error">{error}</p>}
      {!loading && !error && items.length === 0 && <p className="wz-hint">{t("wizard.official.empty")}</p>}
      {!loading && !loggedIn && <p className="wz-hint">{t("wizard.official.loginRequired")}</p>}

      {stage === "select" && items.length > 0 && (
        <>
          <div className="wz-role-list">
            {items.map((candidate) => (
              <label key={candidate.id} className="wz-role-option">
                <input
                  type="radio"
                  name="official-item"
                  value={candidate.id}
                  checked={selected === candidate.id}
                  onChange={() => chooseItem(candidate.id)}
                />
                <span className="wz-role-info">
                  <span className="wz-role-name">{candidate.agent_name ?? candidate.id}</span>
                  <span className="wz-role-charter">{candidate.description}</span>
                  <span className="wz-role-meta">{t("wizard.official.tier")}: {candidate.tier} · v{candidate.version}</span>
                  {!candidate.compatible && <span className="wz-error">{candidate.compatibility_error}</span>}
                </span>
              </label>
            ))}
          </div>
          <div className="wz-role-actions">
            <button
              className="btn btn--primary"
              disabled={!item || !item.compatible || installing || !loggedIn}
              onClick={beginInstall}
            >
              {installing
                ? t("wizard.official.installing")
                : item?.model_requirements
                  ? t("wizard.official.choosePolicy")
                  : t("wizard.official.install")}
            </button>
          </div>
        </>
      )}

      {stage === "policy" && item && (
        <div className="wz-role-list">
          <h3>{t("wizard.official.policyTitle")}</h3>
          <p className="wz-hint">{t("wizard.official.policyHint")}</p>
          {(["capability-first", "cost-first", "privacy-first"] as ModelPolicy[]).map((value) => (
            <label key={value} className="wz-role-option">
              <input
                type="radio"
                name="official-policy"
                value={value}
                checked={policy === value}
                onChange={() => void preview(value)}
              />
              <span className="wz-role-info">
                <span className="wz-role-name">{t(`wizard.official.policy.${value}`)}</span>
                <span className="wz-role-charter">{t(`wizard.official.policy.${value}.hint`)}</span>
              </span>
            </label>
          ))}
          {planning && <p className="wz-progress-text">{t("wizard.official.planning")}</p>}
          {plan && (
            <OfficialPlanPreview
              plan={plan}
              labels={{
                primary: t("wizard.official.primary"),
                fallbacks: t("wizard.official.fallbacks"),
                warning: (kind) => t(`wizard.official.warning.${kind}` as TranslationKey),
              }}
            />
          )}
          <div className="wz-role-actions">
            <button className="btn" onClick={() => { setStage("select"); setError(null); }}>{t("wizard.back")}</button>
            <button
              className="btn btn--primary"
              disabled={!plan || planning || installing || !loggedIn}
              onClick={() => void install({ Automatic: policy })}
            >
              {installing ? t("wizard.official.installing") : t("wizard.official.confirm")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
