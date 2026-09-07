import type React from "react";
import { useEffect, useState } from "react";
import type { AgentDetail, McpNetView, PathGrantView, PermissionsView } from "../../../types";
import { useT } from "../../../i18n";
import { enforcementTone, permCommands } from "./permissionsModel";

/** Spec 2026-09-07 §1.3: enforcement first, runtime traffic and MCP servers
 *  as two blocks, then filesystem / processes / tools / LLM / limits. Read-only. */
export function PermissionsTab({ detail }: { detail: AgentDetail }) {
  const { t } = useT();
  const v = detail.permissions;
  const cmd = permCommands(detail.agent_name);
  return (
    <div className="tab-form perm">
      <EnforcementBanner v={v} />

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
        <p className="perm__mode">{v.runtime_outbound.mode}</p>
        <Outbound v={v} />
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
        {v.filesystem.read.length + v.filesystem.write.length + v.filesystem.deny.length === 0 ? (
          <p className="field-muted perm__muted">{t("perm.fs.none")}</p>
        ) : (
          <>
            <Grants label={t("perm.fs.read")} list={v.filesystem.read} />
            <Grants label={t("perm.fs.write")} list={v.filesystem.write} />
            <Grants label={t("perm.fs.deny")} list={v.filesystem.deny} />
            {v.grants_drifted && <p className="perm__note perm__note--attention">{t("perm.drifted")}</p>}
          </>
        )}
      </Block>

      <Block title={t("perm.processes")} cmd={cmd.spawn}>
        <p className="perm__mode">{t("perm.spawn.mode", { mode: v.processes.spawn_mode })}</p>
        {v.processes.allowed.length > 0 && <Paths list={v.processes.allowed} />}
        {v.processes.allowed_dirs.length > 0 && (
          <>
            <p className="field-muted perm__muted">{t("perm.spawn.dirs")}</p>
            <Paths list={v.processes.allowed_dirs} />
          </>
        )}
      </Block>

      <Block title={t("perm.tools")} cmd={cmd.tools}>
        {v.tools.length === 0 ? (
          <p className="field-muted perm__muted">{t("perm.tools.none")}</p>
        ) : (
          <ul className="perm__list">
            {v.tools.map((r) => (
              <li key={r.pattern} className="perm__row">
                <code>{r.pattern}</code>
                <span className={`perm__policy perm__policy--${r.policy}`}>{r.policy}</span>
                {r.risk && <span className="badge-sm">{r.risk}</span>}
              </li>
            ))}
          </ul>
        )}
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

function Outbound({ v }: { v: PermissionsView }) {
  const { t } = useT();
  const o = v.runtime_outbound;
  if (o.mode === "off") return <p className="field-muted perm__muted">{t("perm.outbound.off")}</p>;
  if (o.mode === "unrestricted") return <p className="field-muted perm__muted">{t("perm.outbound.unrestricted")}</p>;
  if (o.allow_hosts.length === 0) return <p className="field-muted perm__muted">{t("perm.outbound.onlyModel")}</p>;
  return (
    <>
      <Paths list={o.allow_hosts} />
      {o.model_host_always_allowed && <p className="field-muted perm__muted">{t("perm.outbound.plusModel")}</p>}
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

function Grants({ label, list }: { label: string; list: PathGrantView[] }) {
  const { t } = useT();
  if (list.length === 0) return null;
  return (
    <>
      <p className="field-muted perm__muted">{label}</p>
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
          </li>
        ))}
      </ul>
    </>
  );
}

function Paths({ list }: { list: string[] }) {
  return (
    <ul className="perm__list">
      {list.map((p) => <li key={p} className="perm__row"><code>{p}</code></li>)}
    </ul>
  );
}
