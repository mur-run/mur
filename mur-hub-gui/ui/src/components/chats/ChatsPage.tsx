import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useT } from "../../i18n";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { useConversations } from "../../conversation/ConversationContext";
import { SourceList } from "../shell/SourceList";
import { ListDivider } from "../shell/ListDivider";
import {
  LIST_WIDTH_DEFAULT, LIST_WIDTH_MAX, LIST_WIDTH_MIN, useResizableColumn,
} from "../shell/useResizableColumn";
import { listModeFor } from "../shell/breakpoints";
import { useWindowWidth } from "../shell/useWindowWidth";
import { readKey, writeKey } from "../shell/persist";
import { buildChatList, chatFacets, chatRows } from "./chatList";
import { ChatPane, popOutChat } from "./ChatPane";

export const LAST_SELECTED_CHAT_KEY = "mur.chats.lastSelected";
export const CHATS_LIST_WIDTH_KEY = "mur.chats.listWidth";
const ROW_AVATAR_PX = 28;

interface Props {
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  /** The dashboard's channel summaries (useChannels); the primary channel per agent feeds rows and header. */
  channels: ChannelSummary[];
  /** Agent to open when entering from elsewhere (Agents → Chat). */
  initialAgent?: string | null;
  /** Called once the request is applied, so the same agent can be requested again. */
  onInitialHandled?: () => void;
  /** Reports the selection up for ⌘↩ (spec 3(a) §7). */
  onSelect?: (name: string | null) => void;
  onOpenAgent: (name: string) => void;
}

/** Chats (spec 3(a)): SourceList of agents | divider | ChatPane. */
export function ChatsPage({ agents, runtimeMap, channels, initialAgent, onInitialHandled, onSelect, onOpenAgent }: Props) {
  const { t } = useT();
  const { attention, open, openConversation, focusConversation, blurConversation } = useConversations();
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [facet, setFacet] = useState<string | null>(null);
  const [listShown, setListShown] = useState(false);
  // One-shot restore, the AgentsPage pattern: never re-fills after Esc.
  const restored = useRef(false);
  const column = useResizableColumn(CHATS_LIST_WIDTH_KEY, LIST_WIDTH_DEFAULT, LIST_WIDTH_MIN, LIST_WIDTH_MAX);
  const listMode = listModeFor(useWindowWidth());

  useEffect(() => {
    if (restored.current || agents.length === 0) return;
    restored.current = true;
    if (selected !== null) return;
    const last = readKey(LAST_SELECTED_CHAT_KEY);
    if (last && agents.some((a) => a.name === last)) setSelected(last);
  }, [agents, selected]);
  useEffect(() => {
    // Only after the restore ran: the mount render's null must not erase
    // the stored selection before the restore effect has read it.
    if (restored.current) writeKey(LAST_SELECTED_CHAT_KEY, selected);
  }, [selected]);

  // An explicit request (Agents → Chat) outranks the stored selection.
  useEffect(() => {
    if (!initialAgent) return;
    restored.current = true;
    setSelected(initialAgent);
    onInitialHandled?.();
  }, [initialAgent, onInitialHandled]);

  useEffect(() => {
    onSelect?.(selected);
    return () => onSelect?.(null);
  }, [selected, onSelect]);

  // The attention reducer counts deltas / HITL only for open, non-active
  // conversations: open every agent so events register, focus the one being
  // looked at (clearing its flags), and blur when nothing is selected.
  useEffect(() => {
    for (const a of agents) if (!open.includes(a.name)) openConversation(a.name);
    if (selected) focusConversation(selected);
    else blurConversation();
    // `open` is read, not depended on: it changes because of this effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents, selected, openConversation, focusConversation, blurConversation]);

  const items = useMemo(() => buildChatList(agents, attention, channels), [agents, attention, channels]);
  const rows = chatRows(items, runtimeMap, Date.now(), { noChannel: t("chats.noChannel") }, (item) => {
    const preset = avatarPreset(item.agent);
    return <PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={ROW_AVATAR_PX} animate={false} />;
  });
  const facets = chatFacets(items, { needsYou: t("chats.facet.needsYou"), unread: t("chats.facet.unread") });
  const active = items.find((i) => i.name === selected);

  if (agents.length === 0) {
    return <div className="chats-empty"><p>{t("chats.empty")}</p></div>;
  }

  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;
  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={t("nav.chats")}
        count={agents.length}
        rows={rows}
        facets={facets}
        allLabel={t("dashboard.all")}
        activeFacet={facet}
        onFacet={setFacet}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={t("chats.filter")}
        selectedId={selected}
        onSelect={(id) => {
          setSelected(id);
          setListShown(false);
        }}
        onOpen={popOutChat}
        unreadLabel={t("chats.unread")}
        emptyState={<p className="source-list__empty">{t("chats.noMatch")}</p>}
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (
          <button type="button" className="btn btn--secondary master-detail__show-list" onClick={() => setListShown((v) => !v)}>
            {t("shell.showList")}
          </button>
        )}
        {active ? (
          <ChatPane key={active.name} item={active} runtime={runtimeMap.get(active.name)} onOpenAgent={onOpenAgent} />
        ) : (
          <div className="chats-empty"><p>{t("chats.selectHint")}</p></div>
        )}
      </div>
    </div>
  );
}
