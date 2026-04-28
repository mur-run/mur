import { useEffect, useState } from "react";
import { promptGet, promptSet } from "../lib/api";

export default function PromptTab() {
  const [body, setBody] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<string | null>(null);

  useEffect(() => {
    promptGet().then(setBody).catch((e) => setError(String(e)));
  }, []);

  const onSave = async () => {
    setSavedAt(null);
    try {
      await promptSet(body);
      setSavedAt(new Date().toLocaleTimeString());
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-3 h-full flex flex-col">
      <div className="flex justify-between items-center">
        <h2 className="text-lg font-semibold">System Prompt</h2>
        <button
          onClick={onSave}
          className="px-3 py-1 text-sm rounded"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
        >
          Save
        </button>
      </div>
      {error && (
        <div className="text-xs" style={{ color: "var(--color-danger)" }}>{error}</div>
      )}
      {savedAt && (
        <div className="text-xs" style={{ color: "var(--color-success)" }}>Saved at {savedAt}. Restart agent to apply.</div>
      )}
      <textarea
        className="flex-1 font-mono text-xs p-2 border rounded resize-none bg-transparent"
        style={{ borderColor: "var(--color-border)", color: "var(--color-fg)" }}
        value={body}
        onChange={(e) => setBody(e.target.value)}
      />
    </div>
  );
}
