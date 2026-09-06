import type { AgentDetail } from "../../../types";
import type { ModelOption } from "../../modelPicker";
import { useT } from "../../../i18n";
import { ModelCombobox } from "../../ModelCombobox";
import { ModelLibrary } from "../../ModelLibrary";
import { FallbackChainEditor } from "../../settings/FallbackChainEditor";
import { PersonaTab } from "../../inspector/tabs/PersonaTab";
import { StyleTab } from "../../inspector/tabs/StyleTab";
import { BehaviorTab } from "../../inspector/tabs/BehaviorTab";

export interface IdentityTabProps {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
  modelOptions: ModelOption[];
  agentChain: string[];
  chainErr: string | null;
  onChain: (next: string[]) => void;
  agentSmart: boolean | null;
  onSmart: (value: string) => void;
  libraryOpen: boolean;
  setLibraryOpen: (v: boolean) => void;
}

/** Identity = model + persona + style + behavior (spec §4.3). The model block
 *  is the one AgentInspector used to render above the Persona form; the three
 *  tabs are unchanged and stacked as anchored sections. */
export function IdentityTab(p: IdentityTabProps) {
  const { t } = useT();
  return (
    <>
      <section className="detail-section" id="agent-model">
        <h3 className="detail-section__title">{t("detail.section.model")}</h3>
        <ModelCombobox
          detail={p.detail}
          onSaved={p.onSaved}
          onManage={() => p.setLibraryOpen(true)}
        />
        <div className="tab-form" style={{ marginBottom: 18 }}>
          <label className="field-label">{t("detail.fallbackChain")}</label>
          <FallbackChainEditor
            chain={p.agentChain}
            options={p.modelOptions}
            primaryRef={p.detail.model_ref}
            onChange={p.onChain}
            emptyHintKey="detail.fallbackChainInherits"
          />
          {p.chainErr && (
            <p className="settings-hint slot-error">
              {t("detail.fallbackChainError", { error: p.chainErr })}
            </p>
          )}
          <label className="field-label" htmlFor="agent-smart">
            {t("detail.smartRouting")}
          </label>
          <select
            id="agent-smart"
            value={p.agentSmart === null ? "follow" : p.agentSmart ? "on" : "off"}
            onChange={(e) => p.onSmart(e.target.value)}
          >
            <option value="follow">{t("detail.smartFollow")}</option>
            <option value="on">{t("detail.smartOn")}</option>
            <option value="off">{t("detail.smartOff")}</option>
          </select>
          <p className="settings-hint">{t("detail.smartHint")}</p>
        </div>
        <ModelLibrary open={p.libraryOpen} onClose={() => p.setLibraryOpen(false)} />
      </section>
      <section className="detail-section" id="agent-persona">
        <h3 className="detail-section__title">{t("detail.persona")}</h3>
        <PersonaTab detail={p.detail} onSaved={p.onSaved} />
      </section>
      <section className="detail-section" id="agent-style">
        <h3 className="detail-section__title">{t("detail.style")}</h3>
        <StyleTab detail={p.detail} onSaved={p.onSaved} />
      </section>
      <section className="detail-section" id="agent-behavior">
        <h3 className="detail-section__title">{t("detail.behavior")}</h3>
        <BehaviorTab detail={p.detail} onSaved={p.onSaved} />
      </section>
    </>
  );
}
