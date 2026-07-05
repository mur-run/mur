import { useT } from "../../i18n";

interface AgentPickerProps {
  agents: { name: string }[];
  value: string;
  onChange: (name: string) => void;
}

/** Thin presentational wrapper over a native <select> for choosing which
 * agent a Library action (skill / MCP / plugin install) targets. Reused
 * across Library pages; the page owns the agent list and selection state. */
export function AgentPicker({ agents, value, onChange }: AgentPickerProps) {
  const { t } = useT();

  return (
    <label className="agent-picker">
      <span className="field-muted">{t("library.installTo")}</span>
      <select
        className="agent-picker__select"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={agents.length === 0}
      >
        {agents.map((a) => (
          <option key={a.name} value={a.name}>
            {a.name}
          </option>
        ))}
      </select>
    </label>
  );
}
