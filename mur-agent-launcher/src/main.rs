//! macOS per-agent stub launcher.
//!
//! This binary is copied into every per-agent `.app` stub as
//! `Contents/MacOS/<slug>`. On activation it reads two sidecar files,
//! validates the Host version, and `execv`s the Hub binary with the
//! agent slug. Lifetime: milliseconds. Size budget: < 100 KB.
//!
//! Spec §5.2.

use std::ffi::CString;
use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("mur-agent-launcher: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // ── 1. Locate the stub .app bundle ───────────────────────────────────────
    // argv[0] is Contents/MacOS/<slug>. Walk up: MacOS → Contents → .app root.
    let exe = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?
        .canonicalize()
        .map_err(|e| format!("canonicalize exe: {e}"))?;

    // Contents/MacOS/<slug> → Contents/MacOS → Contents → <stub>.app
    let bundle_root = exe
        .parent() // Contents/MacOS
        .and_then(|p| p.parent()) // Contents
        .and_then(|p| p.parent()) // <stub>.app
        .map(PathBuf::from)
        .ok_or("cannot locate bundle root from exe path")?;

    let resources = bundle_root.join("Contents").join("Resources");

    // ── 2. Read agent slug ────────────────────────────────────────────────────
    let slug =
        read_file_trimmed(&resources.join("agent.txt")).map_err(|e| format!("agent.txt: {e}"))?;

    // ── 3. Read expected Host version ─────────────────────────────────────────
    let expected_version = read_file_trimmed(&resources.join("host_version.txt"))
        .map_err(|e| format!("host_version.txt: {e}"))?;

    // ── 4. Read ~/.mur/host_path ──────────────────────────────────────────────
    let mur_home = mur_home_path();
    let host_path_file = mur_home.join("host_path");
    let host_path_contents = read_file_trimmed(&host_path_file)
        .map_err(|e| format!("~/.mur/host_path: {e} (is Hub running?)"))?;

    // host_path file format: "<path>\n<version>"
    let mut lines = host_path_contents.splitn(2, '\n');
    let host_exe = lines
        .next()
        .filter(|s| !s.is_empty())
        .ok_or("host_path is empty")?;
    let recorded_version = lines.next().unwrap_or("").trim();

    // ── 5. Version mismatch → trigger stub regeneration ───────────────────────
    if recorded_version != expected_version {
        // Hub will regenerate the stub then re-launch the agent.
        // Use execv so this process is replaced (no zombie).
        let args = vec![
            host_exe.to_string(),
            "--regenerate-stub".to_string(),
            slug.clone(),
        ];
        execv(host_exe, &args)?;
        unreachable!();
    }

    // ── 6. Build Hub binary path ──────────────────────────────────────────────
    // host_exe is the Hub's Mach-O binary (e.g.
    // /Applications/MuR Agent Host.app/Contents/MacOS/mur-host).
    // We exec it directly (not via `open -b`), preserving argv.

    // ── 7. Detect invocation mode and build args ──────────────────────────────
    let mut hub_args = vec![host_exe.to_string(), "--agent".to_string(), slug.clone()];

    // URL activation: stub was opened via `open muragent-<slug>://<rest>`
    // macOS passes the URL as argv[1] when launched via CFBundleURLTypes.
    let argv: Vec<String> = std::env::args().collect();
    if let Some(url) = argv.get(1) {
        if url.starts_with(&format!("muragent-{slug}://")) || url.starts_with("muragent-") {
            hub_args.push("--url".to_string());
            hub_args.push(url.clone());
        }
    }

    // NSServices share: Hub's NSServices handler writes a temp file and sets
    // MUR_SHARE_FILE in the environment before re-execing the launcher.
    if let Ok(share_file) = std::env::var("MUR_SHARE_FILE") {
        if !share_file.is_empty() {
            hub_args.push("--share-from-file".to_string());
            hub_args.push(share_file);
        }
    }

    // ── 8. execv Hub binary ───────────────────────────────────────────────────
    execv(host_exe, &hub_args)?;
    unreachable!();
}

/// Replace current process with `program` and `args` via POSIX execv.
fn execv(program: &str, args: &[String]) -> Result<(), String> {
    let c_program = CString::new(program).map_err(|e| format!("CString program: {e}"))?;
    let c_args: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()).map_err(|e| format!("CString arg: {e}")))
        .collect::<Result<_, _>>()?;
    let mut argv_ptrs: Vec<*const libc::c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    let _ret = unsafe { libc::execv(c_program.as_ptr(), argv_ptrs.as_ptr()) };
    // execv only returns on failure (ret == -1)
    Err(format!(
        "execv({program}) failed: {}",
        std::io::Error::last_os_error()
    ))
}

fn read_file_trimmed(path: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn mur_home_path() -> PathBuf {
    if let Some(p) = std::env::var_os("MUR_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".mur")
}
