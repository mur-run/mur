import { invoke } from "@tauri-apps/api/core";
import type { AgentRuntimeStatus } from "../../types";
import { useT } from "../../i18n";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { ChatTab } from "../ChatTab";
import { TaskPill } from "../chat/TaskPill";
import { DetailHeader } from "../shell/DetailHeader";
import { statusOf } from "../shell/Status";
import { showToast } from "../detail/fleet/fleetActions";
import type { ChatListItem } from "./chatList";

const POPOUT_ERROR_TOAST_MS = 4000;
const HEADER_AVATAR_PX = 48;

/** The chat's "open in window": the existing chat window (spec 3(a) §7).
 *  Header button, ⌘↩, and row double-click all come here. */
export function popOutChat(name: string): void {
  invoke("open_chat_window", { agentName: name }).catch((err) => showToast(String(err), POPOUT_ERROR_TOAST_MS));
}

export interface ChatPaneProps {
  item: ChatListItem;
  runtime: AgentRuntimeStatus | undefined;
  onOpenAgent: (name: string) => void;
}

/** Header (the inspector's three facts, from data already on the page) plus
 *  the conversation with the live task pill, as in the chat window. */
export function ChatPane({ item, runtime, onOpenAgent }: ChatPaneProps) {
  const { t } = useT();
  const preset = avatarPreset(item.agent);
  const meta = (
    <>
      <span className="mono">{item.agent.model_id}</span>
      <span className="sep">·</span>
      {item.channelId ? (
        <>
          <span className="mono">{item.channelId}</span>
          <span className="sep">·</span>
          <span>{t("chatInspector.turns", { count: item.turns ?? 0 })}</span>
        </>
      ) : (
        <span>{t("chats.noChannel")}</span>
      )}
    </>
  );
  return (
    <section className="chat-pane">
      <DetailHeader
        avatar={<PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={HEADER_AVATAR_PX} />}
        title={item.displayName}
        status={statusOf(runtime?.state)}
        meta={meta}
        actions={
          <>
            <button type="button" className="btn btn--secondary" onClick={() => popOutChat(item.name)}>
              {t("chats.popout")}
            </button>
            <button type="button" className="btn btn--secondary" onClick={() => onOpenAgent(item.name)}>
              {t("chats.openAgent")}
            </button>
          </>
        }
      />
      <div className="chat-pane__body">
        <ChatTab agentName={item.name} displayName={item.displayName} aboveCompose={<TaskPill agentName={item.name} />} />
      </div>
    </section>
  );
}
