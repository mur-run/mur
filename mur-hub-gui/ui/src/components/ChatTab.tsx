//! Two-way chat with an agent (H1). Talks to the `agent_chat_send` Tauri
//! command, which dials the agent over A2A `message/send`. Multi-turn context
//! is kept by threading the previous turn's task id back into the next call.

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

export function ChatTab({ agentName, displayName }: Props) {
  const { t } = useT();
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const taskIdRef = useRef<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  // Fresh conversation when the selected agent changes.
  useEffect(() => {
    setMessages([]);
    taskIdRef.current = null;
  }, [agentName]);

  // Keep the latest message in view.
  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, busy]);

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
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
      setMessages((m) => [...m, { role: "agent", text: res.reply }]);
    } catch (e) {
      setMessages((m) => [...m, { role: "error", text: String(e) }]);
    } finally {
      setBusy(false);
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }

  return (
    <div className="chat">
      <div className="chat__log">
        {messages.length === 0 && !busy && (
          <div className="chat__empty">{t("chat.empty", { name: displayName })}</div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`chat__msg chat__msg--${m.role}`}>
            {m.text}
          </div>
        ))}
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
          disabled={busy || !input.trim()}
        >
          {t("chat.send")}
        </button>
      </div>
    </div>
  );
}
