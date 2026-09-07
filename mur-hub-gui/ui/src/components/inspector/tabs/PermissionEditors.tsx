import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentDetail, ToolPolicy } from "../../../types";
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import { OUTBOUND_MODES, SPAWN_MODES, TOOL_POLICIES, afterWriteHint } from "./permissionsModel";

/** One write path for every editor (spec §P2): invoke a `agent_perm_*`
 *  command, hand the fresh detail up, say whether a restart is needed. The
 *  Tauri side makes exactly one CLI call per command, so the CLI's own
 *  validation runs and its error text is what lands in `error`. */
export interface PermWrite {
  busy: boolean;
  error: string | null;
  hint: ReturnType<typeof afterWriteHint> | null;
  run: (cmd: string, args: Record<string, string>) => Promise<void>;
}

export function usePermWrite(
  detail: AgentDetail,
  onSaved: (d: AgentDetail) => void,
  isRunning: boolean,
): PermWrite {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hint, setHint] = useState<ReturnType<typeof afterWriteHint> | null>(null);
  async function run(cmd: string, args: Record<string, string>) {
    setError(null);
    setBusy(true);
    try {
      onSaved(await invoke<AgentDetail>(cmd, { name: detail.agent_name, ...args }));
      setHint(afterWriteHint(isRunning));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, hint, run };
}

export function ModeSelect<T extends string>({
  value,
  options,
  labelKey,
  cmd,
  write,
}: {
  value: T;
  options: readonly T[];
  labelKey: "perm.mode" | "perm.spawnMode";
  cmd: string;
  write: PermWrite;
}) {
  const { t } = useT();
  return (
    <select
      className="perm__select"
      value={value}
      disabled={write.busy}
      onChange={(e) => void write.run(cmd, { mode: e.target.value })}
    >
      {options.map((m) => (
        <option key={m} value={m}>{t(`${labelKey}.${m}` as TranslationKey)}</option>
      ))}
    </select>
  );
}

export const OutboundSelect = (p: { value: (typeof OUTBOUND_MODES)[number]; write: PermWrite }) => (
  <ModeSelect value={p.value} options={OUTBOUND_MODES} labelKey="perm.mode" cmd="agent_perm_set_outbound_mode" write={p.write} />
);
export const SpawnSelect = (p: { value: (typeof SPAWN_MODES)[number]; write: PermWrite }) => (
  <ModeSelect value={p.value} options={SPAWN_MODES} labelKey="perm.spawnMode" cmd="agent_perm_set_spawn_mode" write={p.write} />
);

export function RemoveBtn({ onClick, write }: { onClick: () => void; write: PermWrite }) {
  const { t } = useT();
  return (
    <button type="button" className="perm__row-x" title={t("detail.remove")} disabled={write.busy} onClick={onClick}>
      ×
    </button>
  );
}

export function PolicySelect({ value, onChange, write }: { value: ToolPolicy; onChange: (p: ToolPolicy) => void; write: PermWrite }) {
  return (
    <select
      className={`perm__select perm__policy perm__policy--${value}`}
      value={value}
      disabled={write.busy}
      onChange={(e) => onChange(e.target.value as ToolPolicy)}
    >
      {TOOL_POLICIES.map((p) => <option key={p} value={p}>{p}</option>)}
    </select>
  );
}

/** Text input + Add: hosts and tool rules. */
export function AddHost({ write }: { write: PermWrite }) {
  const { t } = useT();
  const [host, setHost] = useState("");
  async function add() {
    if (!host.trim()) return;
    await write.run("agent_perm_allow_host", { host: host.trim() });
    setHost("");
  }
  return (
    <form className="perm__add" onSubmit={(e) => { e.preventDefault(); void add(); }}>
      <input className="input" value={host} placeholder={t("perm.hostPlaceholder")} onChange={(e) => setHost(e.target.value)} />
      <button type="submit" className="btn btn--sm btn--secondary" disabled={write.busy || !host.trim()}>{t("perm.addHost")}</button>
    </form>
  );
}

export function AddRule({ write }: { write: PermWrite }) {
  const { t } = useT();
  const [pattern, setPattern] = useState("");
  const [policy, setPolicy] = useState<ToolPolicy>("allow");
  async function add() {
    if (!pattern.trim()) return;
    await write.run("agent_perm_set_tool", { pattern: pattern.trim(), policy });
    setPattern("");
  }
  return (
    <form className="perm__add" onSubmit={(e) => { e.preventDefault(); void add(); }}>
      <input className="input" value={pattern} placeholder={t("perm.patternPlaceholder")} onChange={(e) => setPattern(e.target.value)} />
      <PolicySelect value={policy} onChange={setPolicy} write={write} />
      <button type="submit" className="btn btn--sm btn--secondary" disabled={write.busy || !pattern.trim()}>{t("perm.addRule")}</button>
    </form>
  );
}

/** Native pickers: a folder for a grant list, a program, a build-lane dir.
 *  The picker only offers paths that exist, which is most of what
 *  `reject_dead_grant` would refuse; `reject_ungrantable` still runs on
 *  the Rust side and its message shows verbatim. */
async function pickPath(opts: { directory: boolean; title: string }): Promise<string | null> {
  const picked = await open({ multiple: false, ...opts }).catch(() => null);
  return typeof picked === "string" && picked ? picked : null;
}

export function AddFolder({ verb, write }: { verb: "read" | "write" | "deny"; write: PermWrite }) {
  const { t } = useT();
  async function add() {
    const path = await pickPath({ directory: true, title: t("perm.pickFolder", { verb: t(`perm.fs.${verb}`) }) });
    if (path) await write.run("agent_perm_grant_path", { verb, path });
  }
  return (
    <div className="perm__add">
      <button type="button" className="btn btn--sm btn--secondary" disabled={write.busy} onClick={() => void add()}>{t("perm.addFolder")}</button>
    </div>
  );
}

export function AddProgram({ write }: { write: PermWrite }) {
  const { t } = useT();
  async function add() {
    const program = await pickPath({ directory: false, title: t("perm.pickProgram") });
    if (program) await write.run("agent_perm_allow_spawn", { program });
  }
  return (
    <div className="perm__add">
      <button type="button" className="btn btn--sm btn--secondary" disabled={write.busy} onClick={() => void add()}>{t("perm.addProgram")}</button>
    </div>
  );
}

export function AddDir({ write }: { write: PermWrite }) {
  const { t } = useT();
  async function add() {
    const dir = await pickPath({ directory: true, title: t("perm.pickDir") });
    if (dir) await write.run("agent_perm_allow_spawn_dir", { dir });
  }
  return (
    <div className="perm__add">
      <button type="button" className="btn btn--sm btn--secondary" disabled={write.busy} onClick={() => void add()}>{t("perm.addDir")}</button>
    </div>
  );
}
