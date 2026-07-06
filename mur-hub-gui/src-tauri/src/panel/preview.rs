//! Panel preview backend: read a file for preview rendering, and watch
//! it for external changes so the panel can auto-refresh.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Preview files larger than this are rejected to keep the panel snappy.
const MAX_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct PreviewFile {
    pub kind: &'static str,
    pub content: String,
    pub path: String,
}

pub(crate) fn kind_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md") | Some("markdown") => "markdown",
        Some("html") | Some("htm") => "html",
        _ => "text",
    }
}

pub(crate) fn read_preview(path: &Path) -> Result<PreviewFile, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err(format!(
            "file exceeds {MAX_PREVIEW_BYTES} bytes preview limit"
        ));
    }
    let content = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(PreviewFile {
        kind: kind_of(path),
        content,
        path: path.display().to_string(),
    })
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
    let Some(path) = path else {
        return Ok(());
    };
    let target = PathBuf::from(&path);
    // Watch the parent dir (editors often replace files atomically, which
    // would orphan a file-level watch), filter events to our target.
    let dir = target.parent().unwrap_or(Path::new(".")).to_path_buf();
    let watch_target = target.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && ev.paths.iter().any(|p| p == &watch_target)
        {
            let _ = app.emit(
                "panel-preview-changed",
                serde_json::json!({ "path": watch_target.display().to_string() }),
            );
        }
    })
    .map_err(|e| e.to_string())?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(|e| e.to_string())?;

    *slot = Some(watcher);
    Ok(())
}

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
    }

    #[test]
    fn read_rejects_oversized() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big.md");
        let data = vec![b'x'; (MAX_PREVIEW_BYTES + 1) as usize];
        std::fs::write(&file, data).unwrap();
        let err = read_preview(&file).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn read_returns_content_and_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("doc.md");
        std::fs::write(&file, "# Hello").unwrap();
        let f = read_preview(&file).unwrap();
        assert_eq!(f.kind, "markdown");
        assert_eq!(f.content, "# Hello");
    }
}
