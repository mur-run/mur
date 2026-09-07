import type React from "react";
import { useEffect, useState } from "react";
import type { AgentDetail, McpNetView, PathGrantView, PermissionsView } from "../../../types";
import { useT } from "../../../i18n";
import { enforcementTone, outboundModeForCli, permCommands } from "./permissionsModel";
import {
  AddDir, AddFolder, AddHost, AddProgram, AddRule, OutboundSelect, PolicySelect, RemoveBtn, SpawnSelect,
  usePermWrite, type PermWrite,
} from "./PermissionEditors";

/** Spec 2026-09-07 §1.3 + §P2: enforcement first, runtime traffic and MCP
 *  servers as two blocks, then filesystem / processes / tools / LLM / limits.
 *  Every editable row writes through one CLI call (PermissionEditors). */
export function PermissionsTab({
  detail,
  onSaved,
  isRunning,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
  isRunning: boolean;
}) {
  const { t } = useT();
  const v = detail.permissions;
  const cmd = permCommands(detail.agent_name);
  const write = usePermWrite(detail, onSaved, isRunning);
  return (
    <div className="tab-form perm">
      <EnforcementBanner v={v} />
      {write.error && <p className="save-error">{write.error}</p>}
      {write.hint && <p className="perm__hint field-muted">{t(write.hint)}</p>}

      <label className="field-label">{t("detail.capabilities")}</label>
      {detail.capabilities.length === 0 ? (
        <p className="field-muted perm__muted">{t("detail.noCaps")}</p>
      ) : (
        <div className="badge-row">
          {detail.capabilities.map((c) => (
            <span key={c} className="cap-tag"><span className="cap-dot" />{c}</span>
          ))}
        </div>
      )}

      <Block title={t("perm.runtime")} cmd={cmd.hosts}>
        <p className="field-muted perm__muted">{t("perm.runtime.note")}</p>
        <OutboundSelect value={outboundModeForCli(v.runtime_outbound.mode)} write={write} />
        <Outbound v={v} write={write} />
      </Block>

      <Block title={t("perm.mcp")} cmd={v.mcp_servers.length ? cmd.mcp : undefined}>
        {v.mcp_servers.length === 0 ? (
          <p className="field-muted perm__muted">{t("perm.mcp.none")}</p>
        ) : (
          <ul className="perm__list">
            {v.mcp_servers.map((m) => <McpRow key={m.name} m={m} />)}
          </ul>
        )}
      </Block>

      <Block title={t("perm.filesystem")} cmd={cmd.paths}>
        <Grants verb="read" list={v.filesystem.read} write={write} />
        <Grants verb="write" list={v.filesystem.write} write={write} />
        <Grants verb="deny" list={v.filesystem.deny} write={write} />
        {v.grants_drifted && <p className="perm__note perm__note--attention">{t("perm.drifted")}</p>}
      </Block>

      <Block title={t("perm.processes")} cmd={cmd.spawn}>
        <SpawnSelect value={v.processes.spawn_mode} write={write} />
        <Paths list={v.processes.allowed} onRemove={(program) => void write.run("agent_perm_deny_spawn", { program })} write={write} />
        <AddProgram write={write} />
        <p className="field-muted perm__muted">{t("perm.spawn.dirs")}</p>
        <Paths list={v.processes.allowed_dirs} onRemove={(dir) => void write.run("agent_perm_deny_spawn_dir", { dir })} write={write} />
        <AddDir write={write} />
      </Block>

      <Block title={t("perm.tools")} cmd={cmd.tools}>
        {v.tools.length === 0 ? (
          <p className="field-muted perm__muted">{t("perm.tools.none")}</p>
        ) : (
          <ul className="perm__list">
            {v.tools.map((r) => (
              <li key={r.pattern} className="perm__row">
                <code>{r.pattern}</code>
                <PolicySelect
                  value={r.policy}
                  write={write}
                  onChange={(policy) => void write.run("agent_perm_set_tool", { pattern: r.pattern, policy })}
                />
                {r.risk && <span className="badge-sm">{r.risk}</span>}
                <RemoveBtn write={write} onClick={() => void write.run("agent_perm_clear_tool", { pattern: r.pattern })} />
              </li>
            ))}
          </ul>
        )}
        <AddRule write={write} />
      </Block>

      <Block title={t("perm.limits")}>
        <p className="perm__mode">{t("perm.llm", { mode: v.llm })}</p>
        <p className="field-muted perm__muted">
          {t("perm.limits.value", {
            memory: v.limits.memory_mb,
            fds: v.limits.file_descriptors,
            procs: v.limits.processes,
          })}
          {v.limits.cpu_seconds != null && t("perm.limits.cpu", { cpu: v.limits.cpu_seconds })}
        </p>
        <p className="field-muted perm__muted">
          {v.fail_closed_on_sandbox_error ? t("perm.failClosed.on") : t("perm.failClosed.off")}
        </p>
      </Block>
    </div>
  );
}

function EnforcementBanner({ v }: { v: PermissionsView }) {
  const { t } = useT();
  const tone = enforcementTone(v.enforcement);
  return (
    <p className={`perm__banner perm__banner--${tone}`} role="status">
      {t(`perm.enforcement.${v.enforcement}`, { mode: v.sandbox_mode ?? "" })}
    </p>
  );
}

function Block({ title, cmd, children }: { title: string; cmd?: string; children: React.ReactNode }) {
  return (
    <section className="perm__block">
      <label className="field-label">{title}</label>
      {children}
      {cmd && <CopyCmd cmd={cmd} />}
    </section>
  );
}

/** How long the "copied" acknowledgement stays up. */
const COPIED_MS = 1200;

/** Click the command to copy it. `navigator.clipboard` needs a user gesture,
 *  which a real click has — verified in the Hub's WebView, so this needs no
 *  Tauri clipboard plugin. The text stays selectable for a manual copy. */
function CopyCmd({ cmd }: { cmd: string }) {
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <p className="perm__cmd">
      <button
        type="button"
        className="perm__cmd-btn"
        title={t("perm.copy")}
        onClick={() => {
          navigator.clipboard.writeText(cmd).then(() => setCopied(true)).catch(console.error);
        }}
      >
        <span className="field-muted">{copied ? t("perm.copied") : t("perm.cmdHint")}</span>{" "}
        <code>{cmd}</code>
      </button>
    </p>
  );
}

function Outbound({ v, write }: { v: PermissionsView; write: PermWrite }) {
  const { t } = useT();
  const o = v.runtime_outbound;
  if (o.mode === "off") return <p className="field-muted perm__muted">{t("perm.outbound.off")}</p>;
  if (o.mode === "unrestricted") return <p className="field-muted perm__muted">{t("perm.outbound.unrestricted")}</p>;
  return (
    <>
      {o.allow_hosts.length === 0 ? (
        <p className="field-muted perm__muted">{t("perm.outbound.onlyModel")}</p>
      ) : (
        <>
          <Paths list={o.allow_hosts} onRemove={(host) => void write.run("agent_perm_deny_host", { host })} write={write} />
          {o.model_host_always_allowed && <p className="field-muted perm__muted">{t("perm.outbound.plusModel")}</p>}
        </>
      )}
      <AddHost write={write} />
    </>
  );
}

function McpRow({ m }: { m: McpNetView }) {
  const { t } = useT();
  const detail =
    m.scope === "unbounded" ? t("perm.mcp.unbounded")
    : m.scope === "own_hosts" ? (m.allow_hosts.length ? t("perm.mcp.own_hosts", { hosts: m.allow_hosts.join(", ") }) : t("perm.mcp.own_hosts_none"))
    : m.scope === "all_audited" ? (m.deny_hosts.length ? t("perm.mcp.all_audited_except", { hosts: m.deny_hosts.join(", ") }) : t("perm.mcp.all_audited"))
    : t("perm.mcp.off");
  return (
    <li className={`perm__row${m.scope === "unbounded" ? " perm__row--attention" : ""}`}>
      <code>{m.name}</code>
      <span className="badge-sm">{m.mode}</span>
      <span className="perm__detail">{detail}</span>
    </li>
  );
}

/** One grant list with its verified glyphs, a remove on every row, and the
 *  folder picker under it — shown even when the list is empty, or there is
 *  no way to grant the first folder. */
function Grants({ verb, list, write }: { verb: "read" | "write" | "deny"; list: PathGrantView[]; write: PermWrite }) {
  const { t } = useT();
  return (
    <>
      <p className="field-muted perm__muted">{t(`perm.fs.${verb}`)}</p>
      {list.length > 0 && (
        <ul className="perm__list">
          {list.map((g) => (
            <li key={g.raw} className={`perm__row perm__row--${g.status.status}`} title={g.expanded}>
              <span className="perm__glyph" aria-hidden>
                {g.status.status === "effective" ? "✓" : g.status.status === "dropped" ? "✗" : "·"}
              </span>
              <code>{g.raw}</code>
              {g.status.status === "dropped" && (
                <span className="perm__detail">{t("perm.fs.dropped", { reason: g.status.reason })}</span>
              )}
              <RemoveBtn write={write} onClick={() => void write.run("agent_perm_remove_path", { verb, path: g.raw })} />
            </li>
          ))}
        </ul>
      )}
      <AddFolder verb={verb} write={write} />
    </>
  );
}

function Paths({ list, onRemove, write }: { list: string[]; onRemove: (p: string) => void; write: PermWrite }) {
  if (list.length === 0) return null;
  return (
    <ul className="perm__list">
      {list.map((p) => (
        <li key={p} className="perm__row">
          <code>{p}</code>
          <RemoveBtn write={write} onClick={() => onRemove(p)} />
        </li>
      ))}
    </ul>
  );
}
