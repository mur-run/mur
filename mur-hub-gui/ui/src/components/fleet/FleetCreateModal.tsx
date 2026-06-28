import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";

interface Props {
  onCreated: (name: string) => void;
  onClose: () => void;
}

export function FleetCreateModal({ onCreated, onClose }: Props) {
  const { t } = useT();
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [members, setMembers] = useState("");
  const [router, setRouter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setBusy(true);
    const memberList = members
      .split(",")
      .map((m) => m.trim())
      .filter(Boolean);
    try {
      await invoke("fleet_create", {
        name: name.trim(),
        goal: goal.trim(),
        members: memberList,
        router: router.trim() || null,
      });
      onCreated(name.trim());
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-card" onClick={(e) => e.stopPropagation()}>
        <h2>{t("fleet.new")}</h2>
        <form onSubmit={handleSubmit}>
          <label className="field">
            <span>{t("fleet.create.name")}</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="dev-squad"
              required
              pattern="[a-z0-9_-]+"
              title="Lowercase letters, digits, - or _"
              autoFocus
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.goal")}</span>
            <input
              value={goal}
              onChange={(e) => setGoal(e.target.value)}
              placeholder="Ship the v3 release"
              required
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.members")}</span>
            <input
              value={members}
              onChange={(e) => setMembers(e.target.value)}
              placeholder="pm, qa, dev"
              required
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.router")}</span>
            <input
              value={router}
              onChange={(e) => setRouter(e.target.value)}
              placeholder="mur"
            />
          </label>
          {error && <p className="field-error">{error}</p>}
          <div className="modal-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              {t("detail.close")}
            </button>
            <button
              type="submit"
              className="toolbar-btn toolbar-btn--primary"
              disabled={busy}
            >
              {busy ? "…" : t("fleet.create.submit")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
