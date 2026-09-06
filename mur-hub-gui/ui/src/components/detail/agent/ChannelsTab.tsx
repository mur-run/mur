import { useT } from "../../../i18n";
import { CompanionInbox } from "../../CompanionInbox";
import { MobileTab } from "../../MobileTab";

/** Channels = companion inbox + mobile (spec §4.3). */
export function ChannelsTab({ agentName }: { agentName: string }) {
  const { t } = useT();
  return (
    <>
      <section className="detail-section" id="agent-inbox">
        <h3 className="detail-section__title">{t("detail.inbox")}</h3>
        <CompanionInbox agentName={agentName} />
      </section>
      <section className="detail-section" id="agent-mobile">
        <h3 className="detail-section__title">{t("detail.mobile")}</h3>
        <MobileTab agentName={agentName} />
      </section>
    </>
  );
}
