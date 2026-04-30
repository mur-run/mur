// Model tab: list registry, switch active model, set keychain secret.
// CRUD on the registry stays in the CLI (`mur model add/remove`); this
// tab is read-only for the entry shape.

import { useEffect, useState } from "react";
import {
  getActiveModelRef,
  listModels,
  type ModelEntryView,
  setActiveModelRef,
  setSecret,
} from "../lib/api";

export default function ModelTab() {
  const [entries, setEntries] = useState<ModelEntryView[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    Promise.all([listModels(), getActiveModelRef()])
      .then(([m, a]) => {
        setEntries(m);
        setActive(a);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  };

  useEffect(() => {
    reload();
  }, []);

  if (error) return <pre className="p-4 text-sm">{error}</pre>;

  if (entries.length === 0) {
    return (
      <div className="p-4">
        <h2 className="text-lg font-semibold mb-2">Model</h2>
        <p className="text-sm">
          No models in registry. Add one via CLI:
        </p>
        <pre className="mt-2 text-xs p-2 rounded"
          style={{ background: "var(--color-bg-secondary)" }}>
          mur model add anthropic_opus_4_7 \{"\n"}
          {"  "}--provider anthropic --model claude-opus-4-7 \{"\n"}
          {"  "}--secret keychain:mur-agent/{"<agent>"}/ANTHROPIC_API_KEY
        </pre>
      </div>
    );
  }

  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold">Model</h2>
        <button
          className="text-xs px-2 py-1 border rounded"
          onClick={reload}
        >
          Reload
        </button>
      </div>
      <div className="text-sm">
        Active:{" "}
        <strong>{active ?? "(none — using legacy inline model)"}</strong>
      </div>
      <ul className="space-y-2">
        {entries.map((e) => (
          <li
            key={e.name}
            className="border rounded p-3"
            style={{ borderColor: "var(--color-border)" }}
          >
            <div className="flex items-center gap-2">
              <input
                type="radio"
                name="active-model"
                checked={active === e.name}
                onChange={() =>
                  setActiveModelRef(e.name)
                    .then(reload)
                    .catch((err) => setError(String(err)))}
              />
              <span className="font-medium">{e.name}</span>
              {e.secret_status === false && (
                <span className="text-xs" style={{ color: "var(--color-error, #c33)" }}>
                  ✗ secret not set
                </span>
              )}
              {e.secret_status === true && (
                <span className="text-xs" style={{ color: "var(--color-success, #2a7)" }}>
                  ✓ ready
                </span>
              )}
              {e.secret_status === null && (
                <span className="text-xs" style={{ color: "var(--color-fg-secondary)" }}>
                  no secret needed
                </span>
              )}
            </div>
            <div
              className="text-xs mt-1"
              style={{ color: "var(--color-fg-secondary)" }}
            >
              {e.provider} / {e.model}
              {e.secret_ref && <> · {e.secret_ref}</>}
            </div>
            {e.secret_ref && e.secret_ref.startsWith("keychain:") && (
              <button
                className="text-xs mt-2 px-2 py-1 border rounded"
                onClick={() => setEditing(e.name)}
              >
                {e.secret_status ? "Update" : "Set"} secret
              </button>
            )}
          </li>
        ))}
      </ul>

      {editing && (
        <SecretModal
          entry={entries.find((e) => e.name === editing)!}
          onClose={() => {
            setEditing(null);
            reload();
          }}
          onError={setError}
        />
      )}
    </div>
  );
}

function SecretModal({
  entry,
  onClose,
  onError,
}: {
  entry: ModelEntryView;
  onClose: () => void;
  onError: (e: string) => void;
}) {
  const [value, setValue] = useState("");
  const [show, setShow] = useState(false);

  const submit = () => {
    if (!entry.secret_ref) return;
    setSecret(entry.secret_ref, value)
      .then(() => {
        setValue("");
        onClose();
      })
      .catch((e) => onError(String(e)));
  };

  return (
    <div
      className="fixed inset-0 flex items-center justify-center z-10"
      style={{ background: "rgba(0,0,0,0.4)" }}
    >
      <div
        className="rounded p-4 w-96 shadow-lg"
        style={{
          background: "var(--color-bg)",
          border: "1px solid var(--color-border)",
        }}
      >
        <h3 className="font-semibold mb-2">Set secret for {entry.name}</h3>
        <p
          className="text-xs mb-2"
          style={{ color: "var(--color-fg-secondary)" }}
        >
          Stored in: <code>{entry.secret_ref}</code>
        </p>
        <input
          type={show ? "text" : "password"}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          autoFocus
          className="w-full border rounded px-2 py-1 text-sm mb-2"
          style={{ borderColor: "var(--color-border)" }}
        />
        <label className="text-xs flex items-center gap-1">
          <input
            type="checkbox"
            checked={show}
            onChange={(e) => setShow(e.target.checked)}
          />
          show
        </label>
        <div className="flex gap-2 mt-3 justify-end">
          <button
            className="text-xs px-3 py-1 border rounded"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="text-xs px-3 py-1 rounded"
            style={{
              background: "var(--color-accent)",
              color: "var(--color-accent-fg)",
            }}
            disabled={!value}
            onClick={submit}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
}
