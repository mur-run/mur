import { useEffect, useState } from "react";
import { mcpList, type McpServerEntry } from "../lib/api";

export default function McpTab() {
  const [list, setList] = useState<McpServerEntry[]>([]);

  useEffect(() => {
    mcpList().then(setList).catch(() => {});
  }, []);

  return (
    <div className="space-y-3">
      <h2 className="text-lg font-semibold">MCP Servers</h2>
      <table className="w-full text-sm">
        <thead>
          <tr>
            <th className="text-left p-1">ID</th>
            <th className="text-left p-1">Command</th>
            <th className="text-left p-1">Args</th>
          </tr>
        </thead>
        <tbody>
          {list.map((s) => (
            <tr key={s.id} style={{ borderTop: "1px solid var(--color-border)" }}>
              <td className="p-1 font-mono">{s.id}</td>
              <td className="p-1 font-mono">{s.command}</td>
              <td className="p-1 font-mono text-xs">{s.args.join(" ")}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
