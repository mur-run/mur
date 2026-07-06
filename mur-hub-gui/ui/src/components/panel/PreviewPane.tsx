import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Markdown } from "../Markdown";

type PreviewFile = {
  kind: "markdown" | "html" | "text";
  content: string;
  path: string;
};

const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

function isAllowedUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    return (
      (u.protocol === "http:" || u.protocol === "https:") &&
      LOCAL_HOSTS.has(u.hostname)
    );
  } catch {
    return false;
  }
}

export default function PreviewPane({
  target,
  kind,
}: {
  target: string;
  kind: "file" | "url";
}) {
  const [file, setFile] = useState<PreviewFile | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (kind !== "file") {
      void invoke("panel_watch_preview", { path: null }).catch(() => {});
      return;
    }
    let live = true;
    const load = () =>
      invoke<PreviewFile>("panel_read_preview_file", { path: target })
        .then((f) => {
          if (live) {
            setFile(f);
            setError(null);
          }
        })
        .catch((e) => {
          if (live) setError(String(e));
        });
    load();
    void invoke("panel_watch_preview", { path: target }).catch(() => {});
    const un = listen("panel-preview-changed", () => load());
    return () => {
      live = false;
      un.then((f) => f());
      void invoke("panel_watch_preview", { path: null }).catch(() => {});
    };
  }, [target, kind]);

  if (kind === "url") {
    return isAllowedUrl(target) ? (
      <iframe
        className="preview-frame"
        src={target}
        sandbox="allow-scripts allow-same-origin allow-forms"
        title="preview"
      />
    ) : (
      <p className="panel-empty">Only localhost URLs can be previewed.</p>
    );
  }
  if (error) return <p className="panel-empty">{error}</p>;
  if (!file) return <p className="panel-empty">Loading…</p>;
  if (file.kind === "html")
    return (
      <iframe
        className="preview-frame"
        srcDoc={file.content}
        sandbox="allow-scripts"
        title="preview"
      />
    );
  if (file.kind === "markdown")
    return (
      <div className="preview-md">
        <Markdown>{file.content}</Markdown>
      </div>
    );
  return <pre className="preview-text">{file.content}</pre>;
}
