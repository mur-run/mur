//! Two-way chat with an agent (H1). Talks to the `agent_chat_send` Tauri
//! command, which dials the agent over A2A `message/send`. Multi-turn context
//! is kept by threading the previous turn's task id into the next call. The
//! agent's reply is revealed with a typewriter effect for an alive feel (the
//! runtime has no token streaming yet, so we animate the completed reply).

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";

interface ChatMsg {
  role: "user" | "agent" | "error";
  text: string;
}

interface ChatReply {
  reply: string;
  task_id: string;
}

interface Props {
  agentName: string;
  displayName: string;
}

// How fast the typewriter reveals the reply.
const TYPE_INTERVAL_MS = 18;
const TYPE_CHARS_PER_TICK = 2;

export function ChatTab({ agentName, displayName }: Props) {
  const { t } = useT();
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  // The agent reply currently being typed out, if any.
  const [typing, setTyping] = useState<{ text: string; shown: number } | null>(null);
  const taskIdRef = useRef<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  // Fresh conversation when the selected agent changes.
  useEffect(() => {
    setMessages([]);
    setTyping(null);
    taskIdRef.current = null;
  }, [agentName]);

  // Keep the latest message in view.
  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, busy, typing]);

  // Drive the typewriter: advance the reveal, then commit the finished bubble.
  useEffect(() => {
    if (!typing) return;
    if (typing.shown >= typing.text.length) {
      const finished = typing.text;
      setMessages((m) => [...m, { role: "agent", text: finished }]);
      setTyping(null);
      return;
    }
    const id = setTimeout(() => {
      setTyping((s) =>
        s ? { ...s, shown: Math.min(s.text.length, s.shown + TYPE_CHARS_PER_TICK) } : null,
      );
    }, TYPE_INTERVAL_MS);
    return () => clearTimeout(id);
  }, [typing]);

  async function send() {
    const text = input.trim();
    if (!text || busy || typing) return;
    setInput("");
    setMessages((m) => [...m, { role: "user", text }]);
    setBusy(true);
    try {
      const res = await invoke<ChatReply>("agent_chat_send", {
        name: agentName,
        text,
        contextTaskId: taskIdRef.current,
      });
      taskIdRef.current = res.task_id || taskIdRef.current;
      setBusy(false);
      setTyping({ text: res.reply, shown: 0 }); // reveal with typewriter
    } catch (e) {
      setBusy(false);
      // Keep error bubbles short — the backend may include a runtime log tail.
      const msg = String(e).split("\n")[0].slice(0, 200);
      setMessages((m) => [...m, { role: "error", text: msg }]);
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }

  const idle = messages.length === 0 && !busy && !typing;

  return (
    <div className="chat">
      <div className="chat__log">
        {idle && <div className="chat__empty">{t("chat.empty", { name: displayName })}</div>}
        {messages.map((m, i) => (
          <div key={i} className={`chat__msg chat__msg--${m.role}`}>
            {m.text}
          </div>
        ))}
        {typing && (
          <div className="chat__msg chat__msg--agent">
            {typing.text.slice(0, typing.shown)}
            <span className="chat__caret" />
          </div>
        )}
        {busy && (
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
          disabled={busy || !!typing || !input.trim()}
        >
          {t("chat.send")}
        </button>
      </div>
    </div>
  );
}
