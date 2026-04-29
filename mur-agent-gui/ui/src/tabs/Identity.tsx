import { useEffect, useState } from "react";
import { status, type StatusView } from "../lib/api";

export default function IdentityTab() {
  const [view, setView] = useState<StatusView | null>(null);

  useEffect(() => {
    status().then(setView).catch(() => {});
  }, []);

  return (
    <div className="space-y-3">
      <h2 className="text-lg font-semibold">Identity</h2>
      {view ? (
        <>
          <Row label="Key version" value={String(view.key_version)} />
          <p className="text-xs" style={{ color: "var(--color-fg-secondary)" }}>
            Pubkey + rotation history are wired in P1.3. Use the CLI for now: <code>mur agent rekey-status {view.name}</code>.
          </p>
        </>
      ) : (
        "Loading…"
      )}
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex">
      <div className="w-32 text-sm" style={{ color: "var(--color-fg-secondary)" }}>{label}</div>
      <div className="text-sm font-mono">{value}</div>
    </div>
  );
}
