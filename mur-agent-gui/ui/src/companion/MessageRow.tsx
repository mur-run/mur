import { useState } from "react";
import { ack } from "./api";
import type { BridgeEvent, Signal } from "./types";
import { WhyAccordion } from "./WhyAccordion";

export function MessageRow({
  agent,
  msg,
}: {
  agent: string;
  msg: BridgeEvent;
}) {
  const [acked, setAcked] = useState<Signal | null>(
    msg.response.kind === "signal" ? msg.response.value : null,
  );

  async function send(signal: Signal) {
    setAcked(signal);
    try {
      await ack(agent, msg.id, signal);
    } catch (e) {
      // eslint-disable-next-line no-alert
      alert(`ack failed: ${e}`);
      setAcked(null);
    }
  }

  return (
    <div className="border-b border-neutral-700 p-3 text-sm">
      <div className="text-xs text-neutral-400">
        {msg.situation} · {msg.locale} · {msg.generated_at}
      </div>
      <div className="mt-1 whitespace-pre-wrap">{msg.body}</div>
      <div className="mt-2 flex gap-2 text-base">
        <button
          aria-label="like"
          disabled={acked !== null}
          onClick={() => send("good")}
          className={acked === "good" ? "opacity-50" : ""}
        >
          👍
        </button>
        <button
          aria-label="dislike"
          disabled={acked !== null}
          onClick={() => send("bad")}
          className={acked === "bad" ? "opacity-50" : ""}
        >
          👎
        </button>
        <button
          aria-label="dismiss"
          disabled={acked !== null}
          onClick={() => send("dismiss")}
          className={acked === "dismiss" ? "opacity-50" : ""}
        >
          🚫
        </button>
        {acked && (
          <span className="text-xs text-neutral-500">acked: {acked}</span>
        )}
      </div>
      <WhyAccordion agent={agent} msgId={msg.id} />
    </div>
  );
}
