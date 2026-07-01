import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { groupByProvider, type ModelOption } from "../modelPicker";
import {
  canSubmitMode,
  buildParallelPayload,
  type FleetMode,
  type TrackInput,
} from "./fleetCreateForm";

interface Props {
  onCreated: (name: string) => void;
  onClose: () => void;
}

function ModelSelect({
  models,
  value,
  onChange,
  allowDefault,
}: {
  models: ModelOption[];
  value: string;
  onChange: (v: string) => void;
  allowDefault?: boolean;
}) {
  const { t } = useT();
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      {allowDefault ? (
        <option value="">{t("fleet.create.modelDefault")}</option>
      ) : (
        <option value="" disabled>
          {t("fleet.create.chooseModel")}
        </option>
      )}
      {groupByProvider(models).map(([provider, opts]) => (
        <optgroup key={provider} label={provider}>
          {opts.map((m) => (
            <option key={m.ref_name} value={m.model}>
              {m.model}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );
}

export function FleetCreateModal({ onCreated, onClose }: Props) {
  const { t } = useT();
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [members, setMembers] = useState("");
  const [router, setRouter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [mode, setMode] = useState<FleetMode>("plain");
  const [models, setModels] = useState<ModelOption[]>([]);
  const [judgeModel, setJudgeModel] = useState("");
  const [tracks, setTracks] = useState<TrackInput[]>([]);
  const [preFilterCargoCheck, setPreFilterCargoCheck] = useState(false);
  const [preFilterClippy, setPreFilterClippy] = useState(false);
  const [targetFile, setTargetFile] = useState("");

  useEffect(() => {
    invoke<ModelOption[]>("list_models").then(setModels).catch(() => {});
  }, []);

  function handleModeChange(next: FleetMode) {
    setMode(next);
    if (next === "speculative" && tracks.length === 0) {
      setTracks([
        { name: "track-a", approach: "", model: "" },
        { name: "track-b", approach: "", model: "" },
      ]);
    }
  }

  function updateTrack(i: number, patch: Partial<TrackInput>) {
    setTracks((prev) => prev.map((t, idx) => (idx === i ? { ...t, ...patch } : t)));
  }

  function addTrack() {
    setTracks((prev) => [...prev, { name: `track-${prev.length}`, approach: "", model: "" }]);
  }

  function removeTrack(i: number) {
    setTracks((prev) => prev.filter((_, idx) => idx !== i));
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    if (!canSubmitMode(mode, tracks, judgeModel, targetFile)) {
      setError(t("fleet.create.modeIncomplete"));
      return;
    }
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
        parallel: buildParallelPayload(
          mode,
          tracks,
          judgeModel,
          targetFile,
          preFilterCargoCheck,
          preFilterClippy
        ),
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

          <div className="fleet-create__mode">
            <span className="fleet-section__label">{t("fleet.create.mode.label")}</span>
            {(["plain", "speculative", "partition"] as FleetMode[]).map((m) => (
              <label key={m} className="fleet-create__mode-option">
                <input
                  type="radio"
                  name="fleet-mode"
                  checked={mode === m}
                  onChange={() => handleModeChange(m)}
                />
                <span>
                  {t(`fleet.create.mode.${m}` as Parameters<typeof t>[0])}
                  <span className="fleet-create__mode-desc">
                    {t(`fleet.create.mode.${m}Desc` as Parameters<typeof t>[0])}
                  </span>
                </span>
              </label>
            ))}
          </div>

          {mode === "speculative" && (
            <div className="fleet-create__section">
              <label className="field">
                <span>{t("fleet.create.judgeModel")}</span>
                <ModelSelect models={models} value={judgeModel} onChange={setJudgeModel} />
              </label>
              <span className="fleet-section__label">{t("fleet.create.tracks")}</span>
              {tracks.map((track, i) => (
                <div key={i} className="fleet-create__track">
                  <input
                    value={track.approach}
                    onChange={(e) => updateTrack(i, { approach: e.target.value })}
                    placeholder={t("fleet.create.trackApproach")}
                  />
                  <ModelSelect
                    models={models}
                    value={track.model}
                    onChange={(v) => updateTrack(i, { model: v })}
                    allowDefault
                  />
                  <button type="button" onClick={() => removeTrack(i)}>
                    ✕
                  </button>
                </div>
              ))}
              <button type="button" className="toolbar-btn" onClick={addTrack}>
                {t("fleet.create.addTrack")}
              </button>
              <div className="fleet-create__prefilters">
                <span>{t("fleet.create.preFilter")}</span>
                <label>
                  <input
                    type="checkbox"
                    checked={preFilterCargoCheck}
                    onChange={(e) => setPreFilterCargoCheck(e.target.checked)}
                  />
                  {t("fleet.create.preFilterCargoCheck")}
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={preFilterClippy}
                    onChange={(e) => setPreFilterClippy(e.target.checked)}
                  />
                  {t("fleet.create.preFilterClippy")}
                </label>
              </div>
            </div>
          )}

          {mode === "partition" && (
            <div className="fleet-create__section">
              <label className="field">
                <span>{t("fleet.create.judgeModel")}</span>
                <ModelSelect models={models} value={judgeModel} onChange={setJudgeModel} />
                <span className="fleet-create__mode-desc">{t("fleet.create.judgeModelPartitionHint")}</span>
              </label>
              <label className="field">
                <span>{t("fleet.create.targetFile")}</span>
                <input
                  value={targetFile}
                  onChange={(e) => setTargetFile(e.target.value)}
                  placeholder="src/widget.rs"
                />
                <span className="fleet-create__mode-desc">{t("fleet.create.targetFileHint")}</span>
              </label>
            </div>
          )}

          {error && <p className="field-error">{error}</p>}
          <div className="modal-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              {t("detail.close")}
            </button>
            <button
              type="submit"
              className="toolbar-btn toolbar-btn--primary"
              disabled={busy || !canSubmitMode(mode, tracks, judgeModel, targetFile)}
            >
              {busy ? "…" : t("fleet.create.submit")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
