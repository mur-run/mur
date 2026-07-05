# murmur Panel P3 — Preview Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the Panel's Preview tab: markdown/HTML files (with auto-reload on change) and localhost dev-server URLs in a sandboxed iframe.

**Architecture:** P1 already delivers `panel-preview` events (`{kind: "file"|"url", target}`) to `PanelWindow` — this plan only adds rendering. Backend: two small Tauri commands in `panel/preview.rs` — `panel_read_preview_file` (read + classify a file) and `panel_watch_preview` (single active `notify` watcher that emits `panel-preview-changed`). Frontend: Preview tab renders `.md` via the existing `Markdown.tsx`, `.html` via sandboxed `iframe srcDoc`, and URLs via sandboxed `iframe src` restricted to localhost.

**Tech Stack:** Rust (Tauri 2, notify 6), React/TS (react-markdown already in `ui/package.json`).

**Spec:** Preview section of `docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md` (P3 phase). No new spec needed — scope is exactly the parent spec's Preview paragraph minus `stream {delta}` (P4).

## Global Constraints

- Security (from parent spec): preview URLs restricted to `localhost` / `127.0.0.1` / `[::1]`; iframe sandboxed — no top-navigation, no downloads. HTML files render via `srcDoc` with `sandbox="allow-scripts"` (NO `allow-same-origin`) — scripts run isolated, relative assets don't resolve (documented P3 limit; good enough for agent-produced single-file prototypes).
- Insert-only model untouched — Preview never executes anything on the murmur side.
- murmur (TUI) side: **zero changes** — `/panel preview <path|url>` already ships in P1.
- `cargo fmt` + clippy green per commit; Hub check via `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (stub `ui/dist/index.html` if needed, never commit it).
- File size limit for preview reads: 2 MiB (constant `MAX_PREVIEW_BYTES`); larger files show a "too large" message instead of hanging the webview.

---

### Task 1: Backend — `panel/preview.rs` (read + watch)

**Files:**
- Create: `mur-hub-gui/src-tauri/src/panel/preview.rs`
- Modify: `mur-hub-gui/src-tauri/src/panel/mod.rs` (`pub mod preview;`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register `panel_read_preview_file`, `panel_watch_preview` in `generate_handler!`)
- Modify: `mur-hub-gui/src-tauri/capabilities/panel.json` (allow both commands + the `panel-preview-changed` event, same shape as existing entries)

**Interfaces:**
- Produces:
  - `panel_read_preview_file(path: String) -> Result<PreviewFile, String>` where `PreviewFile { kind: "markdown"|"html"|"text", content: String, path: String }`
  - `panel_watch_preview(app: AppHandle, state: State<PreviewWatch>, path: Option<String>)` — `Some(path)` replaces the single active watcher; `None` stops it. On any change event for the watched file, emits Tauri event `panel-preview-changed` with payload `{ path }`.
  - `PreviewWatch` managed state registered in `lib.rs` via `.manage(preview::PreviewWatch::default())`.

- [ ] **Step 1: Write failing unit tests** (in-file; the pure parts — classification and size guard)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        assert_eq!(kind_of(std::path::Path::new("a.md")), "markdown");
        assert_eq!(kind_of(std::path::Path::new("a.markdown")), "markdown");
        assert_eq!(kind_of(std::path::Path::new("a.html")), "html");
        assert_eq!(kind_of(std::path::Path::new("a.htm")), "html");
        assert_eq!(kind_of(std::path::Path::new("a.rs")), "text");
        assert_eq!(kind_of(std::path::Path::new("noext")), "text");
    }

    #[test]
    fn read_rejects_oversized() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.md");
        std::fs::write(&p, vec![b'x'; (MAX_PREVIEW_BYTES + 1) as usize]).unwrap();
        assert!(read_preview(&p).is_err());
    }

    #[test]
    fn read_returns_content_and_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("doc.md");
        std::fs::write(&p, "# hi").unwrap();
        let f = read_preview(&p).unwrap();
        assert_eq!(f.kind, "markdown");
        assert_eq!(f.content, "# hi");
    }
}
```

- [ ] **Step 2: Run, verify FAIL** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml preview` → compile error.

- [ ] **Step 3: Implement**

```rust
//! Panel P3 preview: file read + single-slot notify watcher.
//! Spec: Preview section of 2026-07-05-murmur-panel-companion-design.md

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

pub const MAX_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct PreviewFile {
    pub kind: &'static str, // "markdown" | "html" | "text"
    pub content: String,
    pub path: String,
}

pub(crate) fn kind_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("md") | Some("markdown") => "markdown",
        Some("html") | Some("htm") => "html",
        _ => "text",
    }
}

pub(crate) fn read_preview(path: &Path) -> Result<PreviewFile, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err(format!("file exceeds {} bytes preview limit", MAX_PREVIEW_BYTES));
    }
    let content = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(PreviewFile { kind: kind_of(path), content, path: path.display().to_string() })
}

#[tauri::command]
pub fn panel_read_preview_file(path: String) -> Result<PreviewFile, String> {
    read_preview(Path::new(&path))
}

/// Single active watcher slot; replacing/stopping drops the previous watcher.
#[derive(Default)]
pub struct PreviewWatch(Mutex<Option<notify::RecommendedWatcher>>);

#[tauri::command]
pub fn panel_watch_preview(
    app: AppHandle,
    state: State<PreviewWatch>,
    path: Option<String>,
) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|e| e.to_string())?;
    *slot = None; // drop old watcher first
    let Some(path) = path else { return Ok(()) };
    let target = PathBuf::from(&path);
    // Watch the parent dir (editors often replace files atomically, which
    // would orphan a file-level watch), filter events to our target.
    let dir = target.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && ev.paths.iter().any(|p| p == &target)
        {
            let _ = app.emit("panel-preview-changed", serde_json::json!({ "path": path }));
        }
    })
    .map_err(|e| e.to_string())?;
    watcher.watch(&dir, RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
    *slot = Some(watcher);
    Ok(())
}
```

Adjust the `Emitter` import to however the existing `panel/mod.rs` emits (`app.emit` vs `emit_to`) — P1's `panel-focus`/`panel-preview` emits in `panel/mod.rs:81-88` are the pattern to copy (targeting the panel window label if that's what P1 does).

- [ ] **Step 4: Run, verify PASS** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml preview` (3 tests). Then full `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.

- [ ] **Step 5: Register in `lib.rs`** (`.manage(panel::preview::PreviewWatch::default())` + both commands in `generate_handler!`) and add to `capabilities/panel.json`. Re-run check.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/panel/ mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/capabilities/panel.json
git commit -m "feat(hub): panel preview backend — file read + notify watcher"
```

---

### Task 2: Frontend — Preview rendering

**Files:**
- Create: `mur-hub-gui/ui/src/components/panel/PreviewPane.tsx`
- Modify: `mur-hub-gui/ui/src/components/panel/PanelWindow.tsx` (replace the P1 placeholder branch at the `tab === "preview"` arm)
- Modify: `mur-hub-gui/ui/src/components/panel/panel.css` (iframe fills pane, `border: 0`)

**Interfaces:**
- Consumes: `panel_read_preview_file`, `panel_watch_preview` commands and `panel-preview-changed` event (Task 1); existing `Markdown` component at `ui/src/components/Markdown.tsx` (check its prop name — likely `children` or `source` — and use accordingly); `previewTarget` state + `panel-preview` listener already in `PanelWindow.tsx:25,47`.
- Produces: `<PreviewPane target={string} kind={"file" | "url"} />`.

- [ ] **Step 1: Write `PreviewPane.tsx`**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import Markdown from "../Markdown";

type PreviewFile = { kind: "markdown" | "html" | "text"; content: string; path: string };

const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

function isAllowedUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    return (u.protocol === "http:" || u.protocol === "https:") && LOCAL_HOSTS.has(u.hostname);
  } catch {
    return false;
  }
}

export default function PreviewPane({ target, kind }: { target: string; kind: "file" | "url" }) {
  const [file, setFile] = useState<PreviewFile | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (kind !== "file") {
      invoke("panel_watch_preview", { path: null }).catch(() => {});
      return;
    }
    let live = true;
    const load = () =>
      invoke<PreviewFile>("panel_read_preview_file", { path: target })
        .then((f) => live && (setFile(f), setError(null)))
        .catch((e) => live && setError(String(e)));
    load();
    invoke("panel_watch_preview", { path: target }).catch(() => {});
    const un = listen("panel-preview-changed", load);
    return () => {
      live = false;
      un.then((f) => f());
      invoke("panel_watch_preview", { path: null }).catch(() => {});
    };
  }, [target, kind]);

  if (kind === "url") {
    return isAllowedUrl(target) ? (
      <iframe className="preview-frame" src={target} sandbox="allow-scripts allow-same-origin allow-forms" title="preview" />
    ) : (
      <p className="panel-empty">Only localhost URLs can be previewed.</p>
    );
  }
  if (error) return <p className="panel-empty">{error}</p>;
  if (!file) return <p className="panel-empty">Loading…</p>;
  if (file.kind === "html")
    return <iframe className="preview-frame" srcDoc={file.content} sandbox="allow-scripts" title="preview" />;
  if (file.kind === "markdown") return <div className="preview-md"><Markdown>{file.content}</Markdown></div>;
  return <pre className="preview-text">{file.content}</pre>;
}
```

(`allow-same-origin` is granted only to localhost dev-server URLs — they need it for their own fetch/HMR; file `srcDoc` deliberately gets `allow-scripts` only. Neither gets `allow-top-navigation` or `allow-downloads`.)

- [ ] **Step 2: Wire into `PanelWindow.tsx`.** The P1 `panel-preview` listener stores only the target string — extend it to store `{kind, target}` (payload already carries `kind`; see `panel/mod.rs:81-88` for the exact payload field names). Replace the placeholder arm:

```tsx
) : tab === "preview" ? (
  preview ? (
    <PreviewPane target={preview.target} kind={preview.kind} />
  ) : (
    <p className="panel-empty">
      No preview target — type <code>/panel preview &lt;path|url&gt;</code> in murmur.
    </p>
  )
```

- [ ] **Step 3: CSS** — `.preview-frame { width: 100%; height: 100%; border: 0; }`, `.preview-md, .preview-text { overflow: auto; height: 100%; }` (adjust to panel.css's existing layout units).

- [ ] **Step 4: Build** — `cd mur-hub-gui/ui && npm run build`. Expected: success.

- [ ] **Step 5: Manual verify** — build the Hub `.app` (per `gotcha_hub_local_app_build_recipe`), run `murmur`:
  - `/panel preview README.md` → rendered markdown; edit the file → pane reloads within ~1 s.
  - `/panel preview some.html` → sandboxed render.
  - `/panel preview http://localhost:5173` (any local dev server) → live page.
  - `/panel preview https://example.com` → "Only localhost URLs" message.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/ui/src/components/panel/
git commit -m "feat(hub-ui): Panel P3 — preview rendering (md/html/localhost URL + auto-reload)"
```

---

### Task 3: Green + docs

- [ ] **Step 1:** `cargo fmt --all` (+ excluded Tauri crates via `--manifest-path`), clippy on the hub manifest, `npm run build`.
- [ ] **Step 2:** Update the panel paragraph in `docs/architecture/runtime-overview.md` (Preview shipped: file + localhost URL + auto-reload; stream render remains P4).
- [ ] **Step 3: Commit** — `git add -A && git commit -m "docs(panel): P3 preview shipped"`

---

## Self-Review

**Spec coverage (parent spec Preview section):** file mode md via existing markdown component ✓ (Task 2), html loaded directly ✓ (srcDoc, sandboxed — the "directly" is satisfied within the stricter sandbox constraint the spec itself demands), notify file-watch reload ✓ (Task 1), dev-server URL restricted to localhost ✓ (Task 2 `isAllowedUrl`), iframe sandboxed no top-nav/downloads ✓, target set by `/panel preview` or clicking a produced-file notification — the click path lands with P2's Notifications tab (out of this plan's scope; `panel-preview` event path is shared).

**Placeholders:** none. **Type consistency:** `PreviewFile.kind` string values consistent across backend serde and TS union; `panel_watch_preview(path: Option<String>)` matches both call sites (`{path: target}` / `{path: null}`).

**Sequencing note:** independent of the P2 plan — only shared file is `PanelWindow.tsx` (different branches of the same match arm); if both plans run, land P2 first and rebase this plan's Task 2 wiring onto the five-tab version.
