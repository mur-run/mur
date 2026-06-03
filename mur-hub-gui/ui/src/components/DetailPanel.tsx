//! Detail panel — right-side slide-in when an agent is selected in the
//! dashboard. Provides 7 tabs: Persona, Style, Behavior, Skills, MCP,
//! Permissions, Inbox.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../types";
import {
  ALL_DETAIL_TABS,
  BUILTIN_PRESETS,
  TAB_LABELS,
  type AgentDetail,
  type DetailPatch,
  type DetailTab,
} from "../types";
import { CompanionInbox } from "./CompanionInbox";

interface Props {
  agentName: string;
  agents: AgentEntry[];
  onClose: () => void;
}

export function DetailPanel({ agentName, agents, onClose }: Props) {
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<DetailTab>("inbox");

  useEffect(() => {
    setError(null);
    setActiveTab("inbox");
    invoke<AgentDetail>("get_agent_detail", { name: agentName })
      .then(setDetail)
      .catch((e) => setError(String(e)));
  }, [agentName]);

  const displayName =
    agents.find((a) => a.name === agentName)?.display_name ?? agentName;

  function handleSaved(updated: AgentDetail) {
    setDetail(updated);
  }

  if (error) {
    return (
      <aside className="detail-panel">
        <div className="detail-panel-header">
          <span className="detail-panel-title">{displayName}</span>
          <button className="detail-panel-close" onClick={onClose}>×</button>
        </div>
        <div className="detail-panel-body">
          <p className="detail-error">Failed to load: {error}</p>
        </div>
      </aside>
    );
  }

  if (!detail) {
    return (
      <aside className="detail-panel">
        <div className="detail-panel-header">
          <span className="detail-panel-title">{displayName}</span>
          <button className="detail-panel-close" onClick={onClose}>×</button>
        </div>
        <div className="detail-panel-body">
          <p className="detail-loading">Loading…</p>
        </div>
      </aside>
    );
  }

  return (
    <aside className="detail-panel">
      <div className="detail-panel-header">
        <span className="detail-panel-title">{detail.display_name}</span>
        <button className="detail-panel-close" onClick={onClose} title="Close">×</button>
      </div>
      <div className="detail-panel-tabs">
        {ALL_DETAIL_TABS.map((tab) => (
          <span
            key={tab}
            className={`detail-tab${activeTab === tab ? " detail-tab--active" : ""}`}
            onClick={() => setActiveTab(tab)}
          >
            {TAB_LABELS[tab]}
          </span>
        ))}
      </div>
      <div className="detail-panel-body">
        {activeTab === "persona" && (
          <PersonaTab detail={detail} onSaved={handleSaved} />
        )}
        {activeTab === "style" && (
          <StyleTab detail={detail} onSaved={handleSaved} />
        )}
        {activeTab === "behavior" && (
          <BehaviorTab detail={detail} onSaved={handleSaved} />
        )}
        {activeTab === "skills" && <SkillsTab detail={detail} />}
        {activeTab === "mcp" && <McpTab detail={detail} />}
        {activeTab === "permissions" && <PermissionsTab detail={detail} />}
        {activeTab === "inbox" && <CompanionInbox agentName={agentName} />}
      </div>
    </aside>
  );
}

// ─── Persona Tab ──────────────────────────────────────────────────────────────

const PERSONA_CATEGORIES = [
  "research", "automation", "monitor", "notify", "commerce", "custom",
];

const TONE_OPTIONS = ["professional", "casual", "friendly", "direct", "playful", "formal"];
const RISK_OPTIONS = ["conservative", "balanced", "bold"];
const VERBOSITY_OPTIONS = ["concise", "balanced", "detailed"];

function PersonaTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const [category, setCategory] = useState(detail.persona_category);
  const [description, setDescription] = useState(detail.persona_description);
  const [tone, setTone] = useState(detail.persona_tone);
  const [risk, setRisk] = useState(detail.persona_risk);
  const [verbosity, setVerbosity] = useState(detail.persona_verbosity);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  async function save() {
    setSaving(true);
    setSaveError(null);
    try {
      const patch: DetailPatch = {
        persona_category: category,
        persona_description: description,
        persona_tone: tone,
        persona_risk: risk,
        persona_verbosity: verbosity,
      };
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch,
      });
      onSaved(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  const changed =
    category !== detail.persona_category ||
    description !== detail.persona_description ||
    tone !== detail.persona_tone ||
    risk !== detail.persona_risk ||
    verbosity !== detail.persona_verbosity;

  return (
    <div className="tab-form">
      <label className="field-label">Category</label>
      <select
        className="input"
        value={category}
        onChange={(e) => { setCategory(e.target.value); }}
      >
        {PERSONA_CATEGORIES.map((c) => (
          <option key={c} value={c}>{c}</option>
        ))}
      </select>

      <label className="field-label">Description</label>
      <textarea
        className="input"
        rows={3}
        value={description}
        onChange={(e) => { setDescription(e.target.value); }}
        placeholder="What this agent does…"
      />

      <label className="field-label">Tone</label>
      <select
        className="input"
        value={tone}
        onChange={(e) => { setTone(e.target.value); }}
      >
        {TONE_OPTIONS.map((t) => (
          <option key={t} value={t}>{t}</option>
        ))}
      </select>

      <label className="field-label">Risk tolerance</label>
      <select
        className="input"
        value={risk}
        onChange={(e) => { setRisk(e.target.value); }}
      >
        {RISK_OPTIONS.map((r) => (
          <option key={r} value={r}>{r}</option>
        ))}
      </select>

      <label className="field-label">Verbosity</label>
      <select
        className="input"
        value={verbosity}
        onChange={(e) => { setVerbosity(e.target.value); }}
      >
        {VERBOSITY_OPTIONS.map((v) => (
          <option key={v} value={v}>{v}</option>
        ))}
      </select>

      <button
        className="toolbar-btn primary save-btn"
        disabled={!changed || saving}
        onClick={save}
      >
        {saving ? "Saving…" : saved ? "✓ Saved" : "Save Changes"}
      </button>
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}

// ─── Style Tab ────────────────────────────────────────────────────────────────

function StyleTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const [selected, setSelected] = useState(detail.style_preset);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(detail.render_status.status === "rendering");

  // Poll for render completion while a render is in flight, refreshing the
  // panel so the status text/progress and preset thumbnails stay current.
  useEffect(() => {
    if (!rendering) return;
    let cancelled = false;
    const started = Date.now();
    const timer = setInterval(async () => {
      try {
        const fresh = await invoke<AgentDetail>("get_agent_detail", {
          name: detail.agent_name,
        });
        if (cancelled) return;
        onSaved(fresh);
        if (fresh.render_status.status === "ready" || fresh.render_status.status === "failed") {
          setRendering(false);
        }
      } catch {
        // transient; keep polling
      }
      if (Date.now() - started > 120_000) setRendering(false); // safety timeout
    }, 1500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [rendering, detail.agent_name, onSaved]);

  async function triggerRender() {
    setSaveError(null);
    setRendering(true);
    try {
      await invoke("render_agent_expressions", { name: detail.agent_name });
    } catch (e) {
      setSaveError(String(e));
      setRendering(false);
    }
  }

  async function pickPreset(id: string) {
    setSelected(id);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { style_preset: id } as DetailPatch,
      });
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  function statusText(): string {
    switch (detail.render_status.status) {
      case "pending": return "Not rendered yet";
      case "rendering": return `Rendering: ${detail.render_status.done}/${detail.render_status.total}`;
      case "ready": return "Ready ✓";
      case "failed": return `Failed: ${detail.render_status.reason}`;
    }
  }

  return (
    <div className="tab-form">
      <label className="field-label">Current style</label>
      <p className="field-value">
        {BUILTIN_PRESETS.find((p) => p.id === selected)?.display_name ?? selected}
        {" "}
        <span className="field-muted">
          ({BUILTIN_PRESETS.find((p) => p.id === selected)?.family ?? "unknown"})
        </span>
      </p>

      <label className="field-label">Render status</label>
      <p className="field-muted" style={{ fontSize: 12 }}>{statusText()}</p>
      {detail.render_status.status === "rendering" && (
        <div className="progress-bar" style={{ marginTop: 6 }}>
          <div
            className="progress-fill"
            style={{
              width: `${(detail.render_status.done / detail.render_status.total) * 100}%`,
            }}
          />
        </div>
      )}
      <button
        className="toolbar-btn"
        style={{ marginTop: 8 }}
        onClick={triggerRender}
        disabled={rendering || detail.render_status.status === "rendering"}
        title="Generate the 12 avatar expressions for this agent"
      >
        {rendering || detail.render_status.status === "rendering"
          ? "Rendering…"
          : detail.render_status.status === "ready"
            ? "Re-render avatar"
            : "Render avatar"}
      </button>

      <label className="field-label" style={{ marginTop: 16 }}>Preset gallery</label>
      <div className="preset-gallery">
        {BUILTIN_PRESETS.map((p) => (
          <button
            key={p.id}
            className={`preset-card${selected === p.id ? " preset-card--active" : ""}`}
            onClick={() => pickPreset(p.id)}
            disabled={saving}
            title={p.description}
          >
            <div className="preset-card-label">{p.display_name}</div>
            <div className="preset-card-family">{p.family}</div>
          </button>
        ))}
      </div>
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}

// ─── Behavior Tab ─────────────────────────────────────────────────────────────

const BEHAVIOR_OPTIONS: { id: string; label: string; desc: string }[] = [
  { id: "quiet", label: "Quiet", desc: "Only speaks when spoken to. No proactive messages." },
  { id: "normal", label: "Normal", desc: "Sends notifications for important events. Balanced." },
  { id: "lively", label: "Lively", desc: "Proactive suggestions, pet animations, chatty." },
];

function BehaviorTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const [selected, setSelected] = useState(detail.behavior_preset);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  async function pick(b: string) {
    setSelected(b);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { behavior_preset: b } as DetailPatch,
      });
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="tab-form">
      <p className="field-muted" style={{ marginBottom: 12, fontSize: 12 }}>
        Controls how proactive the agent is — when it sends messages and how the pet behaves.
      </p>
      {BEHAVIOR_OPTIONS.map((opt) => (
        <label
          key={opt.id}
          className={`radio-card${selected === opt.id ? " radio-card--active" : ""}${saving ? " radio-card--disabled" : ""}`}
        >
          <input
            type="radio"
            name="behavior"
            value={opt.id}
            checked={selected === opt.id}
            onChange={() => pick(opt.id)}
            disabled={saving}
            style={{ display: "none" }}
          />
          <div className="radio-card-label">{opt.label}</div>
          <div className="radio-card-desc">{opt.desc}</div>
        </label>
      ))}
      {saving && <p className="field-muted" style={{ fontSize: 12 }}>Saving…</p>}
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}

// ─── Skills Tab ───────────────────────────────────────────────────────────────

function SkillsTab({ detail }: { detail: AgentDetail }) {
  const hasInstalled = detail.installed_skills.length > 0;
  const hasLegacy = detail.skills.length > 0;

  if (!hasInstalled && !hasLegacy) {
    return (
      <div className="tab-empty">
        <p>No skills installed.</p>
        <p className="field-muted" style={{ fontSize: 12 }}>
          Use <code>mur skill install</code> to add skills to this agent.
        </p>
      </div>
    );
  }

  return (
    <div className="tab-form">
      {hasInstalled && (
        <>
          <label className="field-label">Installed Skills ({detail.installed_skills.length})</label>
          <ul className="item-list">
            {detail.installed_skills.map((s) => (
              <li key={s.name} className="item-card">
                <div className="item-card-name">{s.name}</div>
                {s.version && (
                  <span className="badge-sm">{s.version}</span>
                )}
                {s.description && (
                  <div className="item-card-desc">{s.description}</div>
                )}
                {s.category && (
                  <span className="field-muted" style={{ fontSize: 11 }}>{s.category}</span>
                )}
              </li>
            ))}
          </ul>
        </>
      )}

      {hasLegacy && (
        <>
          <label className="field-label" style={{ marginTop: hasInstalled ? 16 : 0 }}>
            Legacy Skill Paths ({detail.skills.length})
          </label>
          <ul className="item-list">
            {detail.skills.map((s) => (
              <li key={s.path} className="item-card">
                <code style={{ fontSize: 11 }}>{s.path}</code>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

// ─── MCP Tab ──────────────────────────────────────────────────────────────────

function McpTab({ detail }: { detail: AgentDetail }) {
  if (detail.mcp_servers.length === 0) {
    return (
      <div className="tab-empty">
        <p>No MCP servers configured.</p>
        <p className="field-muted" style={{ fontSize: 12 }}>
          Use <code>mur agent mcp add</code> to connect tools to this agent.
        </p>
      </div>
    );
  }

  return (
    <div className="tab-form">
      <label className="field-label">MCP Servers ({detail.mcp_servers.length})</label>
      <ul className="item-list">
        {detail.mcp_servers.map((m) => (
          <li key={m.name} className="item-card">
            <div className="item-card-name">{m.name}</div>
            <code className="item-card-code">{m.command}</code>
            {m.args.length > 0 && (
              <div className="item-card-args">
                {m.args.map((a, i) => (
                  <span key={i} className="badge-sm">{a}</span>
                ))}
              </div>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

// ─── Permissions Tab ──────────────────────────────────────────────────────────

function PermissionsTab({ detail }: { detail: AgentDetail }) {
  return (
    <div className="tab-form">
      <label className="field-label">Capabilities</label>
      {detail.capabilities.length === 0 ? (
        <p className="field-muted" style={{ fontSize: 12 }}>No special capabilities declared.</p>
      ) : (
        <div className="badge-row">
          {detail.capabilities.map((c) => (
            <span key={c} className="badge-cap">{c}</span>
          ))}
        </div>
      )}

      <label className="field-label" style={{ marginTop: 16 }}>MCP Servers</label>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {detail.mcp_servers.length === 0
          ? "No MCP servers configured."
          : `${detail.mcp_servers.length} server(s) — see MCP tab for details.`}
      </p>

      <label className="field-label" style={{ marginTop: 16 }}>Skills</label>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {detail.installed_skills.length === 0 && detail.skills.length === 0
          ? "No skills installed."
          : `${detail.installed_skills.length} installed + ${detail.skills.length} legacy — see Skills tab.`}
      </p>

      <p className="field-muted" style={{ marginTop: 24, fontSize: 11, fontStyle: "italic" }}>
        Permissions are declared at agent creation time. Use{" "}
        <code>mur agent update</code> from the CLI to modify entitlements.
      </p>
    </div>
  );
}
