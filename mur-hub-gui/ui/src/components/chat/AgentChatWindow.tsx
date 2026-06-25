import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import type { AgentEntry, AgentRuntimeStatus, RuntimeState } from "../../types";
import { ChatTab } from "../ChatTab";
import { ChatChannelRail } from "./ChatChannelRail";
import { TaskPill } from "./TaskPill";
import { useT } from "../../i18n";

function agentNameFromHash(): string {
  const hash = window.location.hash; // #/chat/<name>
  return decodeURIComponent(hash.slice("#/chat/".length).replace(/\+/g, " "));
}

function runtimeStatus(rt: RuntimeState | undefined): "running" | "failed" | "idle" {
  if (rt?.state === "running") return "running";
  if (rt?.state === "failed") return "failed";
  return "idle";
}

export function AgentChatWindow() {
  const agentName = agentNameFromHash();
  const { t } = useT();
  const [displayName, setDisplayName] = useState(agentName);
  const [status, setStatus] = useState<"running" | "failed" | "idle">("idle");
  const [activeChannelId, setActiveChannelId] = useState<string | null>(null);

  const expandToFull = () =>
    void getCurrentWindow().setSize(new LogicalSize(780, 660)).catch(() => {});

  // Load display name from agent list.
  useEffect(() => {
    invoke<AgentEntry[]>("list_agents")
      .then((agents) => {
        const entry = agents.find((a) => a.name === agentName);
        if (entry) setDisplayName(entry.display_name);
      })
      .catch(() => {});
  }, [agentName]);

  // Track runtime status for the title bar indicator.
  useEffect(() => {
    const un = listen<AgentRuntimeStatus[]>("runtime-status-changed", (e) => {
      const mine = e.payload.find((s) => s.name === agentName);
      setStatus(runtimeStatus(mine?.state));
    });
    return () => { void un.then((f) => f()); };
  }, [agentName]);

  // Window controls (traffic lights).
  const win = getCurrentWindow();

  return (
    <div className="cw-root">
      {/* Title bar with custom traffic lights */}
      <div className="cw-title" data-tauri-drag-region>
        <div className="cw-title__traffic">
          <button
            className="cw-title__dot cw-title__dot--close"
            onClick={() => void win.close()}
            title="Close"
            aria-label="Close"
          />
          <button
            className="cw-title__dot cw-title__dot--min"
            onClick={() => void win.minimize()}
            title="Minimize"
            aria-label="Minimize"
          />
          <button
            className="cw-title__dot cw-title__dot--max"
            onClick={() => void win.toggleMaximize()}
            title="Maximize"
            aria-label="Maximize"
          />
        </div>
        <div className="cw-title__info">
          <span className="cw-title__avatar" aria-hidden>
            {displayName.split(/\s+/).map(w => w[0] ?? '').join('').slice(0, 2).toUpperCase()}
          </span>
          <span className="cw-title__name">{displayName}</span>
          {activeChannelId && (
            <>
              <span className="cw-title__divider">·</span>
              <span className="cw-title__channel">#{activeChannelId.split("-").slice(-1)[0]}</span>
            </>
          )}
        </div>
        <span className={`cw-title__status cw-title__status--${status}`} title={status} />
        <button
          className="cw-title__dot"
          onClick={expandToFull}
          title={t("chat.expand")}
          aria-label={t("chat.expand")}
          style={{ background: "none", border: "none", cursor: "pointer", padding: "0 4px", fontSize: "14px", lineHeight: 1, color: "var(--text-secondary, #888)" }}
        >⤢</button>
      </div>

      {/* Body: channel rail + main chat */}
      <div className="cw-body">
        <ChatChannelRail
          agentName={agentName}
          activeId={activeChannelId}
          onSelect={setActiveChannelId}
        />

        <div className="cw-main">
          <ChatTab
            agentName={agentName}
            displayName={displayName}
            aboveCompose={<TaskPill agentName={agentName} />}
          />
        </div>
      </div>
    </div>
  );
}
