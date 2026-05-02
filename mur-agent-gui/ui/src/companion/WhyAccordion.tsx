import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export function WhyAccordion({
  agent,
  msgId,
}: {
  agent: string;
  msgId: string;
}) {
  const [open, setOpen] = useState(false);
  const [events, setEvents] = useState<Array<Record<string, unknown>> | null>(
    null,
  );

  async function toggle() {
    setOpen(!open);
    if (!open && events === null) {
      try {
        const result = await invoke<Array<Record<string, unknown>>>(
          "companion_why",
          { agent, msgId },
        );
        setEvents(result);
      } catch (e) {
        console.warn("companion_why failed:", e);
        setEvents([]);
      }
    }
  }

  return (
    <details
      open={open}
      onToggle={toggle}
      className="mt-2 text-xs text-neutral-400"
    >
      <summary className="cursor-pointer">Why did you message?</summary>
      <ol className="mt-1 list-decimal pl-5">
        {events === null ? (
          <li>loading…</li>
        ) : events.length === 0 ? (
          <li>(no ledger entries)</li>
        ) : (
          events.map((ev, i) => (
            <li key={i}>
              <code>{String(ev.event)}</code>
            </li>
          ))
        )}
      </ol>
    </details>
  );
}
