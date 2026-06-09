//! Shared media runtime types + small leaf helpers. Consumed by both `mur-core`
//! (VLC control, media tools) and `mur-agent-runtime` (WatchScheduler), which
//! cannot depend on `mur-core` — so the snapshot-selection and VLC-status-parsing
//! logic that both need lives here rather than being duplicated.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Per-session VLC HTTP connection details. Generated once and persisted so
/// repeated tool calls reach the same running VLC instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlcRuntime {
    pub port: u16,
    pub password: String,
    /// Directory VLC writes snapshots to (`--snapshot-path`).
    pub snapshot_dir: PathBuf,
}

/// Path to the persisted VLC runtime config.
pub fn runtime_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("vlc.json")
}

/// Load the persisted VLC runtime (`vlc.json`), or `None` if absent/unparseable.
/// Used by the runtime supervisor to allowlist VLC's HTTP port in the kernel
/// sandbox, and anywhere else that needs the current VLC connection details.
pub fn load_runtime(mur_home: &Path) -> Option<VlcRuntime> {
    let raw = std::fs::read_to_string(runtime_path(mur_home)).ok()?;
    serde_json::from_str(&raw).ok()
}

// ── VLC status parsing (shared by mur-core's tools and the runtime scheduler) ──

/// Parsed subset of VLC's `requests/status.xml`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VlcStatus {
    pub state: String, // "playing" | "paused" | "stopped"
    pub time: i64,     // seconds elapsed
    pub length: i64,   // seconds total
    pub volume: i64,   // raw VLC volume (256 == 100%)
}

/// Parse the subset of VLC's `status.xml` using a proper XML reader.
/// Missing fields default sensibly.
pub fn parse_status_xml(xml: &str) -> VlcStatus {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut state = "stopped".to_string();
    let mut time = 0i64;
    let mut length = 0i64;
    let mut volume = 0i64;
    let mut buf = Vec::new();
    let mut in_tag = String::new();
    // Element depth: <root> is depth 1, so the top-level playback fields' text
    // sits at depth 2. Gating on this prevents identically-named tags nested
    // deeper in VLC's <information>/<stats> subtrees from clobbering the real
    // top-level values.
    let mut depth = 0i32;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                depth += 1;
                in_tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
            }
            // Clear the active tag when it closes. Without this, the pretty-printed
            // whitespace VLC emits *after* `</state>` arrives as a Text event while
            // `in_tag` is still "state" and clobbers the value (e.g. state == "\n").
            Ok(Event::End(_)) => {
                depth -= 1;
                in_tag.clear();
            }
            // Self-closing element (e.g. `<state/>`): no text to read; just reset
            // the active tag so a later Text isn't attributed to it.
            Ok(Event::Empty(_)) => {
                in_tag.clear();
            }
            Ok(Event::Text(ref e)) if depth == 2 => {
                let text = e.unescape().unwrap_or_default().to_string();
                match in_tag.as_str() {
                    "state" => state = text,
                    "time" => time = text.trim().parse().unwrap_or(0),
                    "length" => length = text.trim().parse().unwrap_or(0),
                    "volume" => volume = text.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    VlcStatus {
        state,
        time,
        length,
        volume,
    }
}

// ── Snapshot file selection (shared by scene_explain and the runtime scheduler) ──

/// Return the most recently modified regular file in `dir`, if any.
pub fn newest_file(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Like [`newest_file`], but returns `None` when the newest file equals `exclude`
/// — the snapshot that already existed before a capture was requested. This
/// requires a *freshly captured* frame rather than silently falling back to a
/// stale snapshot from a previous session.
///
/// VLC names each snapshot uniquely (`vlcsnap-<timestamp>.png`), so "the newest
/// file is a different path than the pre-capture one" is a reliable freshness
/// signal that is independent of filesystem mtime granularity. (Comparing a file
/// mtime against a wall-clock `SystemTime::now()` would spuriously reject genuinely
/// fresh frames on coarse-mtime volumes — HFS+/exFAT/FAT/network mounts floor mtime
/// to whole seconds, below a sub-second `now()`.)
pub fn newest_file_excluding(dir: &Path, exclude: Option<&Path>) -> Option<PathBuf> {
    let newest = newest_file(dir)?;
    match exclude {
        Some(prev) if newest == prev => None,
        _ => Some(newest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn runtime_path_under_runtime_dir() {
        let p = runtime_path(Path::new("/tmp/h"));
        assert!(p.ends_with("runtime/vlc.json"));
    }

    #[test]
    fn load_runtime_absent_is_none() {
        let home = TempDir::new().unwrap();
        assert!(load_runtime(home.path()).is_none());
    }

    #[test]
    fn load_runtime_roundtrips_port() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("runtime");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            runtime_path(home.path()),
            r#"{"port":61886,"password":"pw","snapshot_dir":"/tmp/s"}"#,
        )
        .unwrap();
        assert_eq!(load_runtime(home.path()).unwrap().port, 61886);
    }

    #[test]
    fn parse_status_extracts_fields() {
        let xml = "<root><volume>256</volume><state>playing</state><time>42</time><length>3600</length></root>";
        let s = parse_status_xml(xml);
        assert_eq!(s.state, "playing");
        assert_eq!(s.time, 42);
        assert_eq!(s.length, 3600);
        assert_eq!(s.volume, 256);
    }

    #[test]
    fn parse_status_pretty_printed_does_not_clobber_state() {
        let xml = "<root>\n  <volume>256</volume>\n  <state>playing</state>\n  <time>42</time>\n  <length>3600</length>\n</root>\n";
        let s = parse_status_xml(xml);
        assert_eq!(s.state, "playing");
        assert_eq!(s.time, 42);
        assert_eq!(s.length, 3600);
        assert_eq!(s.volume, 256);
    }

    #[test]
    fn parse_status_ignores_nested_same_named_tags() {
        let xml = "<root>\n\
            \x20 <state>playing</state>\n\
            \x20 <length>3600</length>\n\
            \x20 <information>\n\
            \x20   <category name=\"meta\">\n\
            \x20     <length>0</length>\n\
            \x20     <state>stopped</state>\n\
            \x20   </category>\n\
            \x20 </information>\n\
            </root>";
        let s = parse_status_xml(xml);
        assert_eq!(s.state, "playing");
        assert_eq!(s.length, 3600);
    }

    #[test]
    fn newest_file_picks_latest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.png"), b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("b.png"), b"b").unwrap();
        assert_eq!(
            newest_file(dir.path()).unwrap().file_name().unwrap(),
            "b.png"
        );
    }

    #[test]
    fn newest_file_excluding_rejects_stale_and_accepts_fresh() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("stale.png"), b"old").unwrap();
        let baseline = newest_file(dir.path());
        assert!(newest_file_excluding(dir.path(), baseline.as_deref()).is_none());
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("fresh.png"), b"new").unwrap();
        assert_eq!(
            newest_file_excluding(dir.path(), baseline.as_deref())
                .unwrap()
                .file_name()
                .unwrap(),
            "fresh.png"
        );
    }
}

/// Whether the user has agreed to proactive interjections this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Consent {
    #[default]
    Unasked,
    Granted,
    Declined,
}

/// Persisted proactive-watch session state. Written by the MCP `watch_*` tools
/// (via `mur-core`) and read by the runtime `WatchScheduler`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WatchSession {
    pub active: bool,
    pub muted: bool,
    pub last_interjection_ms: i64,
    pub last_scene_phash: u64,
    pub consent: Consent,
}

/// Path to the persisted watch session.
pub fn watch_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("watch.json")
}

/// Load the watch session, or a default (all-off) session if absent/unparseable.
pub fn load_watch(mur_home: &Path) -> WatchSession {
    std::fs::read_to_string(watch_path(mur_home))
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_default()
}

/// Persist the watch session atomically (temp + rename).
pub fn save_watch(mur_home: &Path, s: &WatchSession) -> std::io::Result<()> {
    let path = watch_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(s).expect("serialize WatchSession");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn absent_session_is_default_off() {
        let home = TempDir::new().unwrap();
        let s = load_watch(home.path());
        assert!(!s.active);
        assert_eq!(s.consent, Consent::Unasked);
    }

    #[test]
    fn session_roundtrips() {
        let home = TempDir::new().unwrap();
        let s = WatchSession {
            active: true,
            muted: false,
            last_interjection_ms: 123,
            last_scene_phash: 0xABCD,
            consent: Consent::Granted,
        };
        save_watch(home.path(), &s).unwrap();
        assert_eq!(load_watch(home.path()), s);
    }
}
