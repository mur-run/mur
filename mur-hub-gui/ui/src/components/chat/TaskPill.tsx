import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { AgentRuntimeStatus } from "../../types";

interface Task {
  label: string;
}

interface Props {
  agentName: string;
}

function toolLabel(toolName: string): string {
  // "bash" → "bash", "str_replace_editor" → "edit file"
  const short: Record<string, string> = {
    bash: "bash",
    str_replace_editor: "edit file",
    read_file: "read",
    write_to_file: "write",
    browser_action: "browser",
  };
  return short[toolName] ?? toolName.replace(/_/g, " ");
}

export function TaskPill({ agentName }: Props) {
  const [tasks, setTasks] = useState<Task[]>([]);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    // Show running state from agent runtime status
    function fromStatus(s: AgentRuntimeStatus | null) {
      if (!s || s.state.state !== "running") {
        setTasks([]);
        return;
      }
      setTasks([{ label: "running" }]);
    }

    // Runtime status arrives as a batch event with all agents.
    const un = listen<AgentRuntimeStatus[]>("runtime-status-changed", (e) => {
      const mine = e.payload.find((s) => s.name === agentName) ?? null;
      fromStatus(mine);
    });

    // Also show tool calls from streaming chat-delta metadata
    const unDelta = listen<{ agent: string; tool?: string }>("chat-delta", (e) => {
      if (e.payload.agent !== agentName || !e.payload.tool) return;
      const label = toolLabel(e.payload.tool);
      setTasks((prev) => {
        if (prev.some((t) => t.label === label)) return prev;
        return [...prev, { label }];
      });
      setDismissed(false);
    });

    return () => {
      void un.then((f) => f());
      void unDelta.then((f) => f());
      setTasks([]);
    };
  }, [agentName]);

  const visible = tasks.length > 0 && !dismissed;

  return (
    <div className={`cw-task-pill${visible ? "" : " cw-task-pill--hidden"}`}>
      <span className="cw-task-pill__icon">⚡</span>
      <div className="cw-task-pill__tasks">
        {tasks.map((t, i) => (
          <span key={i} className="cw-task-pill__item">
            <span className="cw-task-pill__item-dot" />
            {t.label}
          </span>
        ))}
      </div>
      <button
        className="cw-task-pill__dismiss"
        onClick={() => setDismissed(true)}
        title="Dismiss"
      >
        ×
      </button>
    </div>
  );
}
