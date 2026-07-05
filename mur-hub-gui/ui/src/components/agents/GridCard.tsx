import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { useAgents } from "../../context/AgentContext";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import { useUnreadCount } from "../CompanionInbox";
import { PetFace } from "../PetFace";
import { useT } from "../../i18n";
import { CATEGORY_COLORS, avatarInitials, avatarPreset, familyOf, runtimePill } from "../../utils";

// ─── Shared helpers ────────────────────────────────────────────────────────

function showToast(msg: string, durationMs = 2000) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}


// Monochrome (currentColor) action glyphs — WKWebView ignores font-variant-emoji,
// so emoji codepoints render in color; inline SVG is the only reliable mono path.
export function Ico({ filled, children }: { filled?: boolean; children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="15"
      height="15"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

// ─── GridCard ──────────────────────────────────────────────────────────────

interface GridCardProps {
  agent: AgentEntry;
  runtime: AgentRuntimeStatus | undefined;
  isSelected: boolean;
}

export function GridCard({ agent, runtime, isSelected }: GridCardProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const unread = useUnreadCount(agent.name);
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const pill = runtimePill(runtime?.state);
  const isRunning = runtime?.state.state === "running";
  const isBusy = runtime?.state.state === "restarting";

  // Drag-to-spawn state
  const holdTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [dragging, setDragging] = useState(false);
  const [ghostPos, setGhostPos] = useState({ x: 0, y: 0 });
  const cursorOutsideRef = useRef(false);
  const mouseDownPosRef = useRef({ x: 0, y: 0 });

  async function handleRun() {
    await invoke("start_agent", { name: agent.name }).catch((e) =>
      showToast(t("dashboard.startFailed", { error: String(e) })),
    );
  }
  async function handleStop() {
    await invoke("stop_agent", { name: agent.name }).catch((e) =>
      showToast(t("dashboard.stopFailed", { error: String(e) })),
    );
  }
  async function handleShare() {
    const outPath = await save({
      defaultPath: `${agent.name}.muragent`,
      filters: [{ name: "MUR Agent", extensions: ["muragent"] }],
    });
    if (!outPath) return;
    invoke<string>("export_muragent_file", { name: agent.name, outPath })
      .then(() => showToast(`Exported ${agent.name}.muragent`))
      .catch((e) => showToast(`Export failed: ${e}`, 6000));
  }

  function startHold(e: React.MouseEvent) {
    if (e.button !== 0) return;
    mouseDownPosRef.current = { x: e.clientX, y: e.clientY };
    holdTimer.current = setTimeout(() => {
      holdTimer.current = null;
      setDragging(true);
      setGhostPos({ x: e.screenX, y: e.screenY });
      cursorOutsideRef.current = false;
    }, 300);
  }

  function cancelHold() {
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
  }

  // A natural press-and-drag leaves the card before the hold timer fires; treat
  // that as entering the drag instead of cancelling, or the pet can never spawn.
  function leaveCard(e: React.MouseEvent) {
    if (holdTimer.current && (e.buttons & 1) !== 0) {
      cancelHold();
      setDragging(true);
      setGhostPos({ x: e.screenX, y: e.screenY });
      cursorOutsideRef.current = false;
      return;
    }
    cancelHold();
  }

  useEffect(() => {
    if (!dragging) return;

    function onMove(e: MouseEvent) {
      setGhostPos({ x: e.screenX, y: e.screenY });
    }
    function onLeave() { cursorOutsideRef.current = true; }
    function onEnter() { cursorOutsideRef.current = false; }
    function onUp(e: MouseEvent) {
      setDragging(false);
      // Treat the release as "dropped on the desktop" if it lands outside the Hub
      // window. Decide from the release coordinates vs the window bounds rather than
      // relying on a document `mouseleave`, which does NOT fire during a button-held
      // drag out of the window (macOS captures mouse events to the origin window), so
      // the pet would otherwise never spawn.
      const outsideByBounds =
        e.screenX < window.screenX ||
        e.screenX > window.screenX + window.outerWidth ||
        e.screenY < window.screenY ||
        e.screenY > window.screenY + window.outerHeight;
      if (cursorOutsideRef.current || outsideByBounds) {
        invoke("pet_spawn_at", {
          agentName: agent.name,
          screenX: e.screenX,
          screenY: e.screenY,
        }).catch((err) => showToast(`Pet: ${err}`));
      }
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    document.addEventListener("mouseleave", onLeave);
    document.addEventListener("mouseenter", onEnter);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.removeEventListener("mouseleave", onLeave);
      document.removeEventListener("mouseenter", onEnter);
    };
  }, [dragging, agent.name]);

  return (
    <>
      <div
        className={`grid-card${isSelected ? " grid-card--selected" : ""}${dragging ? " grid-card--dragging" : ""}`}
        style={{ ["--cat" as string]: color }}
        data-agent={agent.name}
        onMouseDown={startHold}
        onMouseUp={cancelHold}
        onMouseLeave={leaveCard}
        onClick={() => setSelected(isSelected ? null : agent.name)}
      >
        <div className="grid-card__head">
          <div className="grid-card__avatar grid-card__avatar--pet" title={avatarPreset(agent)}>
            <PetFace
              presetId={avatarPreset(agent)}
              family={familyOf(avatarPreset(agent))}
              expression="idle"
              size={44}
              animate={false}
            />
            {unread > 0 && (
              <span className="unread-badge">{unread > 99 ? "99+" : unread}</span>
            )}
          </div>
          <div>
            <p className="grid-card__name">{agent.display_name}</p>
            {agent.role && <span className="role-chip">{agent.role}</span>}
            <p className="grid-card__cat">{t(`category.${agent.category}` as Parameters<typeof t>[0])}</p>
          </div>
        </div>
        <div className="grid-card__actions">
          <button
            disabled={isRunning || isBusy}
            onClick={(e) => { e.stopPropagation(); handleRun(); }}
            title={t("dashboard.run")}
            aria-label={t("dashboard.run")}
          >
            <Ico filled><polygon points="6 4 20 12 6 20 6 4" /></Ico>
          </button>
          <button
            disabled={!isRunning && !isBusy}
            onClick={(e) => { e.stopPropagation(); handleStop(); }}
            title={t("dashboard.stop")}
            aria-label={t("dashboard.stop")}
          >
            <Ico filled><rect x="6" y="6" width="12" height="12" rx="1.5" /></Ico>
          </button>
          <button onClick={(e) => { e.stopPropagation(); handleShare(); }} title={t("dashboard.share")} aria-label={t("dashboard.share")}>
            <Ico>
              <path d="M4 12v7a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-7" />
              <polyline points="16 7 12 3 8 7" />
              <line x1="12" y1="3" x2="12" y2="15" />
            </Ico>
          </button>
          <button
            onClick={(e) => {
              e.stopPropagation();
              invoke("open_chat_window", { agentName: agent.name }).catch(console.error);
            }}
            title={t("detail.chat")}
            aria-label={t("detail.chat")}
          >
            <Ico>
              <path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z" />
            </Ico>
          </button>

        </div>
        <div className="grid-card__foot">
          <span className={pill.cls}>
            <span className="pill__dot" />
            {t(pill.key)}
          </span>
          <button
            className="grid-card__settings"
            onClick={(e) => { e.stopPropagation(); setSelected(agent.name); }}
            title={t("dashboard.detail")}
            aria-label={t("dashboard.detail")}
          >
            <Ico>
              <path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z" />
              <circle cx="12" cy="12" r="3" />
            </Ico>
          </button>
        </div>
      </div>

      {dragging && (
        <div
          className="drag-ghost"
          style={{
            position: "fixed",
            left: ghostPos.x - window.screenX - 40,
            top: ghostPos.y - window.screenY - 40,
            pointerEvents: "none",
          }}
        >
          <div className="drag-ghost-avatar" style={{ background: color }}>
            {avatarInitials(agent.display_name)}
          </div>
          <span className="drag-ghost-label">{agent.display_name}</span>
        </div>
      )}
    </>
  );
}
