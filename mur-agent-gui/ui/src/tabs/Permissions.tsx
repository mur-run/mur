import { useEffect, useState } from "react";
import { permView } from "../lib/api";

export default function PermissionsTab() {
  const [view, setView] = useState<unknown>(null);

  useEffect(() => {
    permView().then(setView).catch(() => {});
  }, []);

  return (
    <div className="space-y-3">
      <h2 className="text-lg font-semibold">Permissions</h2>
      <pre className="text-xs font-mono whitespace-pre-wrap">
        {view ? JSON.stringify(view, null, 2) : "Loading…"}
      </pre>
    </div>
  );
}
