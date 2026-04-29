import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { logs as fetchLogs, stats, type StatsView } from "./lib/api";

export default function Logs() {
  const [tail, setTail] = useState<string>("");
  const [statsView, setStatsView] = useState<StatsView | null>(null);
  const [filter, setFilter] = useState("");
  const followRef = useRef<HTMLDivElement>(null);

  // Initial load: pull last 200 lines of stderr.log + stats counters.
  useEffect(() => {
    fetchLogs(200).then(setTail).catch(() => {});
    stats().then(setStatsView).catch(() => {});
  }, []);

  // Subscribe to live sidecar:stdout / sidecar:stderr events emitted
  // by sidecar.rs. Append each line to the tail buffer.
  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    const subscribe = async () => {
      for (const ev of ["sidecar:stdout", "sidecar:stderr"]) {
        const off = await listen<string>(ev, (e) => {
          setTail((prev) => `${prev}\n${e.payload}`);
        });
        unlisteners.push(off);
      }
    };
    subscribe();
    return () => {
      unlisteners.forEach((off) => off());
    };
  }, []);

  // Auto-scroll to bottom on new lines.
  useEffect(() => {
    if (followRef.current) {
      followRef.current.scrollTop = followRef.current.scrollHeight;
    }
  }, [tail]);

  const visible = filter
    ? tail
        .split("\n")
        .filter((l) => l.toLowerCase().includes(filter.toLowerCase()))
        .join("\n")
    : tail;

  return (
    <div
      className="flex flex-col h-full"
      style={{ background: "var(--color-bg)", color: "var(--color-fg)" }}
    >
      <header
        className="px-4 py-2 border-b flex justify-between items-center"
        style={{ borderColor: "var(--color-border)", background: "var(--color-bg-secondary)" }}
      >
        <h1 className="text-base font-semibold">Agent Logs</h1>
        <input
          className="text-xs px-2 py-1 rounded"
          style={{
            border: "1px solid var(--color-border)",
            background: "var(--color-bg)",
            color: "var(--color-fg)",
            minWidth: 200,
          }}
          placeholder="Filter (substring)…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </header>

      <div
        ref={followRef}
        className="flex-1 overflow-auto p-3 font-mono text-xs whitespace-pre-wrap"
      >
        {visible || (
          <span style={{ color: "var(--color-fg_secondary)" }}>(no log output yet)</span>
        )}
      </div>

      {statsView && (
        <footer
          className="border-t px-4 py-2 text-xs"
          style={{ borderColor: "var(--color-border)", background: "var(--color-bg-secondary)" }}
        >
          <div className="flex gap-4 flex-wrap">
            <span>files: {statsView.files_scanned}</span>
            <span>bytes: {statsView.bytes_scanned}</span>
            {Object.entries(statsView.counters)
              .slice(0, 6)
              .map(([k, v]) => (
                <span key={k}>
                  <code>{k}</code>: {v}
                </span>
              ))}
          </div>
        </footer>
      )}
    </div>
  );
}
