import { useEffect, useState } from "react";
import { skillList, skillShow } from "../lib/api";

export default function SkillsTab() {
  const [list, setList] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [body, setBody] = useState("");

  useEffect(() => {
    skillList().then(setList).catch(() => {});
  }, []);

  useEffect(() => {
    if (!selected) return;
    skillShow(selected).then(setBody).catch((e) => setBody(String(e)));
  }, [selected]);

  return (
    <div className="flex h-full gap-4">
      <ul
        className="w-48 border-r overflow-auto pr-2"
        style={{ borderColor: "var(--color-border)" }}
      >
        {list.map((s) => (
          <li key={s}>
            <button
              className="w-full text-left px-2 py-1 text-sm rounded"
              style={{
                background: selected === s ? "var(--color-accent)" : "transparent",
                color: selected === s ? "var(--color-accent-fg)" : "var(--color-fg)",
              }}
              onClick={() => setSelected(s)}
            >
              {s}
            </button>
          </li>
        ))}
      </ul>
      <div className="flex-1 overflow-auto">
        <h2 className="text-lg font-semibold mb-2">{selected ?? "Skills"}</h2>
        <pre className="text-xs whitespace-pre-wrap">{body}</pre>
      </div>
    </div>
  );
}
