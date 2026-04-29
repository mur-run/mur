import { useEffect, useState } from "react";
import { status, listThemes, setTheme as setThemeApi, type StatusView, type ThemeInfo } from "../lib/api";

export default function StatusTab() {
  const [view, setView] = useState<StatusView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [themes, setThemes] = useState<ThemeInfo[]>([]);

  useEffect(() => {
    status().then(setView).catch((e) => setError(String(e)));
    listThemes().then(setThemes).catch(() => {});
  }, []);

  if (error) return <Pre title="Error">{error}</Pre>;
  if (!view) return <Pre title="Loading">Reading agent status…</Pre>;

  return (
    <div className="space-y-4">
      <h2 className="text-lg font-semibold">Status</h2>
      <Field label="Name" value={view.name} />
      <Field label="State" value={view.kind} />
      <Field label="PID" value={String(view.pid ?? "—")} />
      <Field
        label="Uptime"
        value={view.uptime_seconds != null ? `${view.uptime_seconds}s` : "—"}
      />
      <Field label="Key version" value={String(view.key_version)} />
      <Field label="Agent home" value={view.agent_home} />

      <div className="pt-4">
        <h3 className="text-sm font-semibold mb-2">Theme</h3>
        <select
          className="border rounded px-2 py-1 bg-transparent"
          style={{ borderColor: "var(--color-border)", color: "var(--color-fg)" }}
          onChange={(e) => setThemeApi(e.target.value)}
        >
          {themes.map((t) => (
            <option key={t.name} value={t.name}>
              {t.display_name} ({t.kind})
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex">
      <div className="w-32 text-sm" style={{ color: "var(--color-fg-secondary)" }}>{label}</div>
      <div className="text-sm">{value}</div>
    </div>
  );
}

function Pre({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <h2 className="text-lg font-semibold mb-2">{title}</h2>
      <pre className="text-xs font-mono whitespace-pre-wrap">{children}</pre>
    </div>
  );
}
