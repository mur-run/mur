//! Two-way chat with an agent (H1) with live token streaming. The backend
//! `agent_chat_send` command emits `chat-delta` events as the reply generates;
//! we append them to a live bubble, then commit the final reply when the call
//! resolves. Multi-turn context is threaded via the previous turn's task id.

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../i18n";
import { HitlCard } from "./HitlCard";
import type { HitlRequest } from "../types";

interface ChatMsg {
  role: "user" | "agent" | "error";
  text: string;
  /** True when this agent reply was committed early by a Stop action. */
  stopped?: boolean;
}

/** Fresh per-turn task id, sent with each message so a Stop can cancel by it. */
export function newTaskId(): string {
  return `task-${crypto.randomUUID()}`;
}

/** Build the agent message committed when a turn is stopped — the partial
 *  streamed buffer, tagged so the UI can mark it. Returns a stopped marker even
 *  when nothing streamed yet. */
export function buildStoppedMessage(partial: string): ChatMsg {
  return { role: "agent", text: partial, stopped: true };
}

interface ChatReply {
  reply: string;
  task_id: string;
  streamed: boolean;
}

interface ChatDelta {
  agent: string;
  text: string;
  thinking: boolean;
}

interface Props {
  agentName: string;
  displayName: string;
}

export function ChatTab({ agentName, displayName }: Props) {
  const { t } = useT();
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [hitlRequests, setHitlRequests] = useState<HitlRequest[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  // Live answer text accumulating from `chat-delta` events (null when idle).
  const [streaming, setStreaming] = useState<string | null>(null);
  // Live "thinking" reasoning, shown transiently until the answer starts.
  const [thinking, setThinking] = useState<string | null>(null);
  const streamingRef = useRef("");
  const thinkingRef = useRef("");
  const taskIdRef = useRef<string | null>(null);
  // The id of the turn currently in flight (for Stop); null when idle.
  const currentTaskIdRef = useRef<string | null>(null);
  // Set when Stop fired this turn, so the resolving send() doesn't append a
  // second (full or empty) agent bubble after we already committed the partial.
  const stoppedRef = useRef(false);
  const endRef = useRef<HTMLDivElement>(null);
  const thinkRef = useRef<HTMLDivElement>(null);

  // Keep the reasoning box scrolled to the latest line (vertical flow).
  useEffect(() => {
    if (thinkRef.current) thinkRef.current.scrollTop = thinkRef.current.scrollHeight;
  }, [thinking]);

  // Fresh conversation when the selected agent changes.
  useEffect(() => {
    setMessages([]);
    setHitlRequests([]);
    setStreaming(null);
    setThinking(null);
    streamingRef.current = "";
    thinkingRef.current = "";
    taskIdRef.current = null;
  }, [agentName]);

  // Subscribe to token deltas for this agent.
  useEffect(() => {
    const un = listen<ChatDelta>("chat-delta", (e) => {
      if (e.payload.agent !== agentName) return;
      if (e.payload.thinking) {
        thinkingRef.current += e.payload.text;
        setThinking(thinkingRef.current);
      } else {
        streamingRef.current += e.payload.text;
        setStreaming(streamingRef.current);
        // Keep the reasoning visible (collapsed) above the answer once it
        // starts — like DeepSeek's deepthink — rather than dropping it.
      }
    });
    const unHitl = listen<HitlRequest>("hitl-approval-needed", (e) => {
      if (e.payload.agent !== agentName) return;
      setHitlRequests((prev) => [...prev, e.payload]);
    });
    return () => {
      void un.then((f) => f());
      void unHitl.then((f) => f());
    };
  }, [agentName]);

  // Keep the latest content in view.
  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, busy, streaming]);

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setMessages((m) => [...m, { role: "user", text }]);
    setBusy(true);
    streamingRef.current = "";
    thinkingRef.current = "";
    setStreaming("");
    setThinking(null);
    stoppedRef.current = false;
    const taskId = newTaskId();
    currentTaskIdRef.current = taskId;
    try {
      const res = await invoke<ChatReply>("agent_chat_send", {
        name: agentName,
        text,
        taskId,
        contextTaskId: taskIdRef.current,
      });
      taskIdRef.current = res.task_id || taskIdRef.current;
      // If Stop already committed the partial reply, don't append again.
      if (!stoppedRef.current) {
        setMessages((m) => [...m, { role: "agent", text: res.reply }]);
      }
    } catch (e) {
      // A stopped turn resolves as a Cancelled result, sometimes via the error
      // path with no body — the partial is already committed, so stay quiet.
      if (!stoppedRef.current) {
        const msg = String(e).split("\n")[0].slice(0, 200);
        setMessages((m) => [...m, { role: "error", text: msg }]);
      }
    } finally {
      setStreaming(null);
      setThinking(null);
      streamingRef.current = "";
      thinkingRef.current = "";
      currentTaskIdRef.current = null;
      setBusy(false);
    }
  }

  async function stop() {
    if (!busy) return;
    // Snapshot whatever streamed so far and commit it as the reply, tagged
    // "stopped", before asking the backend to cancel generation.
    stoppedRef.current = true;
    const partial = streamingRef.current;
    setMessages((m) => [...m, buildStoppedMessage(partial)]);
    setStreaming(null);
    setThinking(null);
    streamingRef.current = "";
    thinkingRef.current = "";
    try {
      await invoke("agent_chat_cancel", { name: agentName });
    } catch {
      // Benign: the turn may have already finished server-side.
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Escape" && busy) {
      e.preventDefault();
      void stop();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }

  const idle = messages.length === 0 && !busy && streaming === null && thinking === null;
  const hasAnswer = streaming !== null && streaming.length > 0;
  // Show the dots only before anything (thinking or answer) has streamed.
  const awaitingFirstToken = busy && !hasAnswer && (thinking === null || thinking.length === 0);

  function handleSuggest(text: string) {
    setInput(text);
  }

  function handleTextareaChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    setInput(e.target.value);
    e.target.style.height = "auto";
    e.target.style.height = Math.min(e.target.scrollHeight, 120) + "px";
  }

  return (
    <div className="chat">
      <div className="chat__log">
        {idle && (
          <div className="chat__empty-wrap">
            <p className="chat__empty">{t("chat.empty", { name: displayName })}</p>
            <div className="chat__suggestions">
              {(["chat.suggest.0", "chat.suggest.1", "chat.suggest.2"] as const).map((k) => (
                <button
                  key={k}
                  className="chat__suggest-btn"
                  onClick={() => handleSuggest(t(k))}
                >
                  {t(k)}
                </button>
              ))}
            </div>
          </div>
        )}
        {messages.map((m, i) => (
          <div
            key={i}
            className={`chat__msg chat__msg--${m.role}${m.stopped ? " chat__msg--stopped" : ""}`}
          >
            {m.text}
            {m.stopped && <span className="chat__stopped-tag"> · {t("chat.stopped")}</span>}
          </div>
        ))}
        {thinking !== null && thinking.length > 0 && (
          <div className={`chat__think${hasAnswer ? " chat__think--collapsed" : ""}`}>
            <div className="chat__think-label">{t("chat.thinking")}</div>
            <div className="chat__think-scroll" ref={thinkRef}>
              {thinking}
            </div>
          </div>
        )}
        {hasAnswer && (
          <div className="chat__msg chat__msg--agent">
            {streaming}
            <span className="chat__caret" />
          </div>
        )}
        {awaitingFirstToken && (
          <div className="chat__msg chat__msg--agent chat__typing">
            <span className="chat__dot" />
            <span className="chat__dot" />
            <span className="chat__dot" />
          </div>
        )}
        {hitlRequests.map((req) => (
          <HitlCard key={req.hitl_id} request={req} />
        ))}
        <div ref={endRef} />
      </div>
      <div className="chat__compose">
        <textarea
          className="chat__input"
          value={input}
          onChange={handleTextareaChange}
          onKeyDown={onKeyDown}
          placeholder={t("chat.placeholder", { name: displayName })}
          rows={1}
        />
        {busy ? (
          <button
            className="chat__send chat__stop"
            onClick={() => void stop()}
            title={t("chat.stop")}
          >
            {t("chat.stop")}
          </button>
        ) : (
          <button
            className="chat__send"
            onClick={() => void send()}
            disabled={!input.trim()}
          >
            {t("chat.send")}
          </button>
        )}
      </div>
    </div>
  );
}
