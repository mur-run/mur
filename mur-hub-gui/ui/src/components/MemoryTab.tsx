import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { MemoryView, MemoryPatch } from "../types";

interface Props {
  agentName: string;
}

const RELATIONSHIP_OPTIONS = ["friend", "coach", "accountability_buddy", "mentor"] as const;
const FORMALITY_OPTIONS = ["casual", "neutral", "formal"] as const;

export function MemoryTab({ agentName }: Props) {
  const [view, setView] = useState<MemoryView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [promptOpen, setPromptOpen] = useState(false);
  const [promptDraft, setPromptDraft] = useState("");
  const [resetConfirm, setResetConfirm] = useState(false);

  const [relationship, setRelationship] = useState("friend");
  const [formality, setFormality] = useState("neutral");
  const [firstMemory, setFirstMemory] = useState("");

  useEffect(() => {
    setError(null);
    invoke<MemoryView>("agent_get_memory", { name: agentName })
      .then((v) => {
        setView(v);
        setRelationship(v.relationship);
        setFormality(v.formality);
        setFirstMemory(v.first_memory);
        setPromptDraft(v.sys_prompt);
      })
      .catch((e) => setError(String(e)));
  }, [agentName]);

  async function saveRelationship() {
    setSaving(true);
    setError(null);
    try {
      const patch: MemoryPatch = { relationship, formality, first_memory: firstMemory };
      const updated = await invoke<MemoryView>("agent_set_memory", {
        name: agentName,
        patch,
      });
      setView(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  async function savePrompt() {
    setSaving(true);
    setError(null);
    try {
      const updated = await invoke<MemoryView>("agent_set_memory", {
        name: agentName,
        patch: { sys_prompt: promptDraft } as MemoryPatch,
      });
      setView(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  async function resetPrompt() {
    setSaving(true);
    setError(null);
    setResetConfirm(false);
    try {
      const defaultPrompt = await invoke<string>("agent_reset_sys_prompt", {
        name: agentName,
      });
      setPromptDraft(defaultPrompt);
      setView((v) => (v ? { ...v, sys_prompt: defaultPrompt } : v));
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  if (!view) {
    return <div className="tab-form"><p className="field-muted">Loading…</p></div>;
  }

  return (
    <div className="tab-form">
      <div className="memory-section-title">Relationship</div>

      {!view.companion_initialised && (
        <p className="field-muted" style={{ fontSize: 12, marginBottom: 10 }}>
          Companion not initialised — run{" "}
          <code>mur agent companion init {agentName}</code> to enable relationship editing.
        </p>
      )}

      <div className={`memory-fields${!view.companion_initialised ? " memory-fields--disabled" : ""}`}>
        <div className="field-group">
          <label className="field-label">How this agent sees you</label>
          <select
            className="input"
            value={relationship}
            disabled={!view.companion_initialised}
            onChange={(e) => setRelationship(e.target.value)}
          >
            {RELATIONSHIP_OPTIONS.map((r) => (
              <option key={r} value={r}>
                {r.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase())}
              </option>
            ))}
          </select>
        </div>

        <div className="field-group">
          <label className="field-label">Formality</label>
          <select
            className="input"
            value={formality}
            disabled={!view.companion_initialised}
            onChange={(e) => setFormality(e.target.value)}
          >
            {FORMALITY_OPTIONS.map((f) => (
              <option key={f} value={f}>
                {f.charAt(0).toUpperCase() + f.slice(1)}
              </option>
            ))}
          </select>
        </div>

        <div className="field-group">
          <label className="field-label">What it knows about you</label>
          <textarea
            className="input"
            rows={4}
            placeholder="Nothing recorded yet."
            value={firstMemory}
            disabled={!view.companion_initialised}
            onChange={(e) => setFirstMemory(e.target.value)}
          />
        </div>

        <button
          className="btn btn--primary"
          onClick={saveRelationship}
          disabled={saving || !view.companion_initialised}
        >
          Save
        </button>
      </div>

      <div
        className="memory-advanced-header"
        onClick={() => setPromptOpen((o) => !o)}
        role="button"
        tabIndex={0}
      >
        <span>System Prompt (Advanced)</span>
        <span>{promptOpen ? "▾" : "▸"}</span>
      </div>

      {promptOpen && (
        <div className="memory-prompt-editor">
          <p className="field-muted" style={{ fontSize: 11, marginBottom: 8 }}>
            ⚠ Editing the system prompt directly can break this agent&apos;s behaviour.
          </p>
          <textarea
            className="input memory-prompt-textarea"
            rows={10}
            value={promptDraft}
            onChange={(e) => setPromptDraft(e.target.value)}
          />
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            <button
              className="btn btn--primary"
              onClick={savePrompt}
              disabled={saving}
            >
              Save
            </button>
            {!resetConfirm ? (
              <button
                className="btn btn--secondary"
                onClick={() => setResetConfirm(true)}
                disabled={saving}
              >
                Reset to default
              </button>
            ) : (
              <button
                className="btn btn--danger"
                onClick={resetPrompt}
                disabled={saving}
              >
                Confirm reset
              </button>
            )}
          </div>
        </div>
      )}

      {error && <p className="save-error" style={{ marginTop: 8 }}>{error}</p>}
    </div>
  );
}
