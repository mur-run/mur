import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface MobileEvent {
  ts: string;
  name: string;
  payload: { role?: string; text?: string; [key: string]: unknown };
}

interface Props {
  agentName: string;
}

export function MobileTab({ agentName }: Props) {
  const [lines, setLines] = useState<MobileEvent[]>([]);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let active = true;
    let lastLen = 0;

    async function poll() {
      if (!active) return;
      try {
        const raw = await invoke<string>("mobile_events_read", { agentName });
        const events: MobileEvent[] = raw
          .split("\n")
          .filter(Boolean)
          .map((l) => {
            try { return JSON.parse(l) as MobileEvent; } catch { return null; }
          })
          .filter((e): e is MobileEvent => e !== null);
        if (events.length !== lastLen) {
          lastLen = events.length;
          setLines(events);
          setError(null);
        }
      } catch (e) {
        setError(String(e));
      }
      if (active) setTimeout(poll, 2000);
    }

    poll();
    return () => { active = false; };
  }, [agentName]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [lines]);

  if (error) {
    return (
      <div className="mobile-tab mobile-tab--empty">
        <p className="mobile-tab__hint">{error}</p>
      </div>
    );
  }

  if (lines.length === 0) {
    return (
      <div className="mobile-tab mobile-tab--empty">
        <p className="mobile-tab__hint">
          No mobile conversations yet. Pair your phone with{" "}
          <code>mur agent pair</code>.
        </p>
      </div>
    );
  }

  return (
    <div className="mobile-tab">
      {lines.map((ev, i) => {
        const isUser = ev.payload.role === "user" || ev.name === "mobile.transcript";
        const text = ev.payload.text ?? JSON.stringify(ev.payload);
        const time = new Date(ev.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
        return (
          <div
            key={i}
            className={`mobile-tab__bubble mobile-tab__bubble--${isUser ? "user" : "agent"}`}
          >
            <span className="mobile-tab__text">{text}</span>
            <span className="mobile-tab__time">{time}</span>
          </div>
        );
      })}
      <div ref={bottomRef} />
    </div>
  );
}
