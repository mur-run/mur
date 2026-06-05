//! Two-way chat with an agent (H1) with live token streaming. The backend
//! `agent_chat_send` command emits `chat-delta` events as the reply generates;
//! we append them to a live bubble, then commit the final reply when the call
//! resolves. Multi-turn context is threaded via the previous turn's task id.

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../i18n";

interface ChatMsg {
  role: "user" | "agent" | "error";
  text: string;
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
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  // Live answer text accumulating from `chat-delta` events (null when idle).
  const [streaming, setStreaming] = useState<string | null>(null);
  // Live "thinking" reasoning, shown transiently until the answer starts.
  const [thinking, setThinking] = useState<string | null>(null);
  const streamingRef = useRef("");
  const thinkingRef = useRef("");
  const taskIdRef = useRef<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  // Fresh conversation when the selected agent changes.
  useEffect(() => {
    setMessages([]);
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
        // The answer has started — drop the transient thinking trace.
        if (thinkingRef.current) {
          thinkingRef.current = "";
          setThinking(null);
        }
      }
    });
    return () => {
      void un.then((f) => f());
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
    try {
      const res = await invoke<ChatReply>("agent_chat_send", {
        name: agentName,
        text,
        contextTaskId: taskIdRef.current,
      });
      taskIdRef.current = res.task_id || taskIdRef.current;
      setMessages((m) => [...m, { role: "agent", text: res.reply }]);
    } catch (e) {
      const msg = String(e).split("\n")[0].slice(0, 200);
      setMessages((m) => [...m, { role: "error", text: msg }]);
    } finally {
      setStreaming(null);
      setThinking(null);
      streamingRef.current = "";
      thinkingRef.current = "";
      setBusy(false);
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }

  const idle = messages.length === 0 && !busy && streaming === null && thinking === null;
  const hasAnswer = streaming !== null && streaming.length > 0;
  // Show the dots only before anything (thinking or answer) has streamed.
  const awaitingFirstToken = busy && !hasAnswer && (thinking === null || thinking.length === 0);

  return (
    <div className="chat">
      <div className="chat__log">
        {idle && <div className="chat__empty">{t("chat.empty", { name: displayName })}</div>}
        {messages.map((m, i) => (
          <div key={i} className={`chat__msg chat__msg--${m.role}`}>
            {m.text}
          </div>
        ))}
        {!hasAnswer && thinking !== null && thinking.length > 0 && (
          <div className="chat__think">
            <span className="chat__think-label">{t("chat.thinking")}</span>
            <span className="chat__think-text">{thinking.slice(-160)}</span>
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
        <div ref={endRef} />
      </div>
      <div className="chat__compose">
        <textarea
          className="chat__input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder={t("chat.placeholder", { name: displayName })}
          rows={1}
        />
        <button
          className="chat__send"
          onClick={() => void send()}
          disabled={busy || !input.trim()}
        >
          {t("chat.send")}
        </button>
      </div>
    </div>
  );
}
