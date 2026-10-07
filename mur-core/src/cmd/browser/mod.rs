//! `mur browser` command handlers. Heavy implementation lives in `mur-browser`.

mod auth;
pub mod doctor;
pub mod engine_check;
pub mod live_state;
pub mod perms;
mod replay;
pub mod server_install;
pub mod setup;

pub use auth::*;
pub use replay::{parse_heal_ratio, replay};

use std::{fs, path::PathBuf};
#[cfg(unix)]
use std::{os::unix::fs::FileTypeExt, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
#[cfg(test)]
use mur_browser::auth::write_meta;
#[cfg(unix)]
use mur_browser::broker::SocketClient;
#[cfg(unix)]
use mur_browser::broker::{Broker, KeychainStore};
#[cfg(test)]
use mur_browser::recorder::to_yaml;
use mur_browser::{
    auth::{Handoff, ProfileMeta, save_profile},
    export::to_spec_ts,
    paths,
    proxy::{BrokerHook, capture_storage_state, playwright_command, run_stdio},
    recorder::{Mode, RecordHook, Run, from_yaml},
    state::KeychainStateKeyStore,
};

/// Start the transparent Playwright MCP proxy for one recording session.
///
/// Slice 1 deliberately validates only CLI-level inputs then forwards every
/// JSON-RPC line unchanged. Recorder, broker, and replay hooks replace the
/// `ForwardHook` in later slices without changing this MCP launch contract.
#[cfg(unix)]
pub async fn record(
    run: &str,
    profile: Option<&str>,
    mode: &str,
    trace: bool,
    extra: &[String],
) -> Result<()> {
    mur_browser::paths::validate_name(run)?;
    if let Some(profile) = profile {
        mur_browser::paths::validate_name(profile)?;
    }
    let mode = mode.parse::<Mode>()?;

    // Live mode (Gap 0): Chromium ignores HTTP_PROXY, so the runtime's egress
    // token is handed over in a 0600 `--config` file instead. Refusing when
    // the env is absent is deliberate — an unproxied live browser violates D1.
    // `@playwright/mcp` reads the file once at process start
    // (`resolveCLIConfigForMCP` → `loadConfig`), so it is removed when the
    // child exits; there is no earlier point worth a hook.
    let live_config = live_config_for(
        mode,
        std::env::var(mur_browser::live_proxy::PROXY_ENV)
            .ok()
            .as_deref(),
    )?;
    let mut args = match &live_config {
        Some(config) => mur_browser::live_proxy::launch_args(config),
        None => Vec::new(),
    };
    // These flags are intentionally merely forwarded. `@playwright/mcp`
    // owns their validation, keeping this proxy compatible with new releases.
    // They go on before the derived args below so an explicit `--browser` or
    // `--user-data-dir` is visible to them and wins.
    args.extend(extra.iter().cloned());
    // A headed launch on a shell-only cache fails here with both fixes named,
    // before the profile is decrypted, instead of at launch with Playwright's
    // bare "Executable doesn't exist". Checked before the engine default is
    // added, so "did the caller choose" reads only the caller's own flags.
    let install_dir = mur_browser::server::install_dir(&mur_home()?);
    let browsers = mur_browser::chromium::system_browsers_dir();
    // `setup --only-shell` installs the headless shell and NOT the ~180 MiB
    // full build, so "setup succeeded, the browser downloaded" and "record
    // cannot launch" were both true at once. A run that needs no window goes
    // headless and the shell serves it; live mode (a human logging in) still
    // needs the window, so it falls through to the refusal below.
    let auto_headless = mur_browser::engines::auto_headless_args(
        &args,
        &install_dir,
        browsers.as_deref(),
        mode == Mode::Live,
    );
    if !auto_headless.is_empty() {
        tracing::info!(
            run,
            "no full Chromium build; recording headless on the installed shell"
        );
        args.extend(auto_headless);
    }
    engine_check::record_preflight(run, &args, &install_dir, browsers.as_deref())?;
    // Every mode honours `--profile` the same way replay does. Without this the
    // launch had no cookies and every authenticated page bounced to its login
    // form, which no browser-app grant can fix: the state was never passed.
    // This is not live-only: recording a test or automation run against an
    // admin area is exactly the case that needs a session, and asking the
    // person to log in by hand inside each recording defeats `browser auth`.
    let injected = live_state::prepare(&mur_home()?, profile)?;
    let state_args = live_state::args(injected.as_ref(), &args);
    args.extend(state_args);
    // Without this, test/automation recording fell through to
    // `@playwright/mcp`'s default — the branded Google Chrome application,
    // carrying the person's real profile and needing a spawn grant on
    // `/Applications`. Live mode already asks for Chromium, and `--browser`
    // in `extra` still wins, so this only fills the gap.
    args.extend(mur_browser::engines::default_engine_args(
        &args,
        &install_dir,
        browsers.as_deref(),
    ));
    if trace {
        args.push("--save-trace".into());
    }
    // Resolve the server before any broker state exists, so a missing install
    // fails with the setup hint and leaves no socket behind.
    let server = playwright_command(&args)?;
    let run_state = Run {
        name: run.to_string(),
        mode,
        profile: profile.map(ToOwned::to_owned),
        recorded_at: chrono::Utc::now(),
        steps: Vec::new(),
    };
    let mur_home = mur_home()?;
    let socket = paths::broker_socket(&mur_home);
    remove_stale_broker_socket(&socket)?;
    let token = fresh_broker_token();
    let broker = Arc::new(Broker::new(token.clone(), Arc::new(KeychainStore)));
    let socket_for_task = socket.clone();
    let broker_task = tokio::spawn(async move { broker.serve(&socket_for_task).await });

    // Wait until the 0600 socket exists before the proxy can make a secret
    // call. A short bounded wait turns startup races into a clear error.
    if let Err(error) = wait_for_socket(&socket).await {
        broker_task.abort();
        let _ = std::fs::remove_file(&socket);
        return Err(error);
    }

    let hook = BrokerHook::new(
        RecordHook::with_actions_path(run_state, paths::run_actions(&mur_home, run)),
        Arc::new(SocketClient::new(&socket, token)),
    );
    tracing::info!(
        run,
        mode = ?mode,
        live = live_config.is_some(),
        actions = %paths::run_actions(&mur_home, run).display(),
        "browser record started"
    );
    let result = run_stdio(server, hook).await;
    // Removes the token-bearing config directory (no-op for test/automation)
    // and the decrypted session file.
    drop(live_config);
    drop(injected);
    tracing::info!(run, ok = result.is_ok(), "browser record finished");
    // The child has ended; kill the broker and unlink its private endpoint
    // even when Playwright exited with an error.
    broker_task.abort();
    let _ = broker_task.await;
    let _ = std::fs::remove_file(&socket);
    result
}

/// The token-bearing Playwright config for a live session, or `None` for the
/// recording modes. `proxy_env` is the runtime's `HTTPS_PROXY` value; live
/// mode fails closed without it (D1) rather than launching unproxied.
fn live_config_for(
    mode: Mode,
    proxy_env: Option<&str>,
) -> Result<Option<mur_browser::live_proxy::ConfigFile>> {
    match mode {
        Mode::Live => {
            let creds = mur_browser::live_proxy::parse_proxy_url(proxy_env)
                .context("live mode needs the runtime's egress proxy")?;
            Ok(Some(mur_browser::live_proxy::ConfigFile::write(
                &std::env::temp_dir(),
                &creds,
            )?))
        }
        Mode::Test | Mode::Automation => Ok(None),
    }
}

#[cfg(not(unix))]
pub async fn record(
    _run: &str,
    _profile: Option<&str>,
    _mode: &str,
    _trace: bool,
    _extra: &[String],
) -> Result<()> {
    bail!("browser recording requires Unix domain sockets and is unavailable on this platform")
}

fn mur_home() -> Result<std::path::PathBuf> {
    mur_common::home::mur_home_or_err()
}

fn fresh_broker_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(unix)]
fn remove_stale_broker_socket(socket: &std::path::Path) -> Result<()> {
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(socket).map_err(Into::into)
        }
        Ok(_) => bail!(
            "refusing to replace non-socket broker path: {}",
            socket.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
async fn wait_for_socket(socket: &std::path::Path) -> Result<()> {
    for _ in 0..30 {
        if socket.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    bail!("secret broker did not create socket: {}", socket.display())
}

/// Serve the private secret-broker socket. The token must arrive through the
/// environment rather than argv, so shells and process listings never expose it.
#[cfg(unix)]
pub async fn broker() -> Result<()> {
    let token = std::env::var(mur_browser::broker::TOKEN_ENV)
        .map_err(|_| anyhow::anyhow!("{} is required", mur_browser::broker::TOKEN_ENV))?;
    if token.len() < 32 {
        bail!(
            "{} must be an unguessable capability token",
            mur_browser::broker::TOKEN_ENV
        );
    }
    let mur_home = mur_home()?;
    let socket = paths::broker_socket(&mur_home);
    remove_stale_broker_socket(&socket)?;
    let result = Arc::new(Broker::new(token, Arc::new(KeychainStore)))
        .serve(&socket)
        .await;
    let _ = std::fs::remove_file(socket);
    result
}

#[cfg(not(unix))]
pub async fn broker() -> Result<()> {
    bail!("browser secret broker requires Unix domain sockets and is unavailable on this platform")
}

/// List recorded runs under `~/.mur/browser/runs/`.
pub fn list() -> Result<()> {
    let runs = runs_dir()?;
    if !runs.exists() {
        return Ok(());
    }
    let mut names = fs::read_dir(&runs)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.file_name()))
        .filter_map(|name| name.into_string().ok())
        .filter(|name| paths::validate_name(name).is_ok())
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        println!("{name}");
    }
    Ok(())
}

/// Print a recorded run exactly as stored.  Parsing first avoids presenting a
/// malformed file as a valid recording and keeps traversal out of this path.
pub fn show(name: &str) -> Result<()> {
    paths::validate_name(name)?;
    let path = paths::run_actions(&mur_home()?, name);
    let yaml = fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("read browser run {}: {error}", path.display()))?;
    mur_browser::recorder::from_yaml(&yaml)
        .map_err(|error| anyhow::anyhow!("invalid browser run {}: {error}", path.display()))?;
    print!("{yaml}");
    Ok(())
}

/// Export a recorded run as a Playwright `.spec.ts` file.  Writes to `out`
/// when given, otherwise prints the spec to stdout.
pub fn export(name: &str, out: Option<&std::path::Path>) -> Result<()> {
    paths::validate_name(name)?;
    let path = paths::run_actions(&mur_home()?, name);
    let yaml = fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("read browser run {}: {error}", path.display()))?;
    let run = from_yaml(&yaml)
        .map_err(|error| anyhow::anyhow!("invalid browser run {}: {error}", path.display()))?;
    let spec = to_spec_ts(&run)?;
    match out {
        Some(out) => {
            fs::write(out, &spec)
                .map_err(|error| anyhow::anyhow!("write {}: {error}", out.display()))?;
        }
        None => print!("{spec}"),
    }
    Ok(())
}

/// Delete old recorded runs under `~/.mur/browser/runs/`, keeping the `keep`
/// most recently recorded ones (and, if `older_than` is set, only deleting
/// runs older than that many days beyond the `keep` cutoff).
///
/// Conservative by design: a run directory whose `actions.yaml` is missing or
/// fails to parse is always kept, never deleted. `dry_run` prints the
/// would-delete list without touching disk.
pub fn prune(keep: usize, older_than: Option<u32>, dry_run: bool) -> Result<()> {
    let home = mur_home()?;
    let runs_root = runs_dir()?;
    if !runs_root.exists() {
        return Ok(());
    }

    let mut parsed = Vec::new();
    let mut unparsed = Vec::new();
    for name in directory_names(runs_root.clone())? {
        let actions_path = paths::run_actions(&home, &name);
        match fs::read_to_string(&actions_path)
            .ok()
            .and_then(|yaml| from_yaml(&yaml).ok())
        {
            Some(run) => parsed.push((name, run.recorded_at)),
            None => unparsed.push(name),
        }
    }
    for name in &unparsed {
        println!("keeping {name} (unparsable actions.yaml)");
    }

    // Newest first so the first `keep` entries are the ones to retain.
    parsed.sort_by_key(|(_, recorded_at)| std::cmp::Reverse(*recorded_at));

    let cutoff =
        older_than.map(|days| chrono::Utc::now() - chrono::Duration::days(i64::from(days)));

    let to_delete: Vec<&str> = parsed
        .iter()
        .skip(keep)
        .filter(|(_, recorded_at)| cutoff.is_none_or(|cutoff| *recorded_at < cutoff))
        .map(|(name, _)| name.as_str())
        .collect();

    if to_delete.is_empty() {
        return Ok(());
    }

    if dry_run {
        println!("would delete:");
        for name in &to_delete {
            println!("  {name}");
        }
        return Ok(());
    }

    for name in &to_delete {
        let dir = paths::run_dir(&home, name);
        fs::remove_dir_all(&dir)
            .map_err(|error| anyhow::anyhow!("remove browser run {}: {error}", dir.display()))?;
        println!("deleted {name}");
    }
    Ok(())
}

/// Show the locally persisted recording/profile inventory.  No browser is
/// launched, so this remains safe to call from diagnostics and scripts.
pub fn status() -> Result<()> {
    let home = mur_home()?;
    let mut profiles = profile_statuses(&home)?;
    let mut runs = directory_names(runs_dir()?)?;
    profiles.sort();
    runs.sort();
    println!(
        "profiles: {}",
        if profiles.is_empty() {
            "(none)".into()
        } else {
            profiles.join(", ")
        }
    );
    println!(
        "runs: {}",
        if runs.is_empty() {
            "(none)".into()
        } else {
            runs.join(", ")
        }
    );
    Ok(())
}

fn profile_statuses(home: &std::path::Path) -> Result<Vec<String>> {
    directory_names(paths::browser_root(home).join("profiles"))?
        .into_iter()
        .map(|site| {
            let state = paths::profile_state(home, &site);
            let meta_path = paths::profile_meta(home, &site);
            let label = if !state.is_file() {
                format!("{site} (incomplete)")
            } else if !meta_path.is_file() {
                format!("{site} (metadata missing)")
            } else {
                let yaml = fs::read_to_string(&meta_path).map_err(|error| {
                    anyhow::anyhow!(
                        "read browser profile metadata {}: {error}",
                        meta_path.display()
                    )
                })?;
                let meta: ProfileMeta = serde_yaml::from_str(&yaml).map_err(|error| {
                    anyhow::anyhow!(
                        "invalid browser profile metadata {}: {error}",
                        meta_path.display()
                    )
                })?;
                match meta.earliest_cookie_expires {
                    Some(expiry) => format!("{site} (cookie expires {expiry})"),
                    None => format!("{site} (authenticated {})", meta.last_auth),
                }
            };
            Ok(label)
        })
        .collect()
}

fn runs_dir() -> Result<PathBuf> {
    Ok(paths::browser_root(&mur_home()?).join("runs"))
}

fn directory_names(path: PathBuf) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(fs::read_dir(path)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.file_name()))
        .filter_map(|name| name.into_string().ok())
        .filter(|name| paths::validate_name(name).is_ok())
        .collect())
}

#[test]
fn profile_statuses_marks_incomplete_profiles_without_reading_state() {
    let temp = tempfile::tempdir().unwrap();
    let profile = paths::profile_dir(temp.path(), "example");
    fs::create_dir_all(&profile).unwrap();
    assert_eq!(
        profile_statuses(temp.path()).unwrap(),
        vec!["example (incomplete)"]
    );
}

#[test]
fn profile_statuses_reports_cookie_expiry_from_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let profile = paths::profile_dir(temp.path(), "example");
    fs::create_dir_all(&profile).unwrap();
    fs::write(paths::profile_state(temp.path(), "example"), b"encrypted").unwrap();
    write_meta(
        &paths::profile_meta(temp.path(), "example"),
        &ProfileMeta {
            url: "https://example.test/login".into(),
            last_auth: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            earliest_cookie_expires: chrono::DateTime::from_timestamp(1_700_100_000, 0),
            allow_domains: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(
        profile_statuses(temp.path()).unwrap(),
        vec!["example (cookie expires 2023-11-16 02:00:00 UTC)"]
    );
}

#[test]
fn live_mode_fails_closed_without_the_proxy_and_never_touches_record_modes() {
    // Recording modes ignore the proxy env entirely (Chromium never read it).
    assert!(live_config_for(Mode::Test, None).unwrap().is_none());
    assert!(
        live_config_for(Mode::Automation, Some("http://tok:x@127.0.0.1:1"))
            .unwrap()
            .is_none()
    );
    // Live with no proxy: refuse rather than launch an unproxied browser (D1).
    let err = format!("{:#}", live_config_for(Mode::Live, None).unwrap_err());
    assert!(err.contains("HTTPS_PROXY"), "{err}");
    // Live with the runtime's URL: a --config file carrying the token.
    let cfg = live_config_for(Mode::Live, Some("http://tok:x@127.0.0.1:1"))
        .unwrap()
        .unwrap();
    let body = std::fs::read_to_string(cfg.path()).unwrap();
    assert!(body.contains("\"username\": \"tok\""), "{body}");
    let args = mur_browser::live_proxy::launch_args(&cfg);
    assert!(args.contains(&"--config".to_owned()), "{args:?}");
}

#[test]
fn directory_names_ignore_files_invalid_names_and_missing_directories() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("valid-run")).unwrap();
    fs::create_dir(temp.path().join(".hidden")).unwrap();
    fs::write(temp.path().join("not-a-run"), "x").unwrap();
    assert_eq!(
        directory_names(temp.path().to_path_buf()).unwrap(),
        vec!["valid-run"]
    );
    assert!(
        directory_names(temp.path().join("missing"))
            .unwrap()
            .is_empty()
    );
}

/// Write a minimal parseable run under `runs/<name>/actions.yaml`, with a
/// `recorded_at` `offset_minutes` before now — bigger offset is older.
#[cfg(test)]
fn write_test_run(mur_home: &std::path::Path, name: &str, offset_minutes: i64) {
    let run = Run {
        name: name.to_string(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now() - chrono::Duration::minutes(offset_minutes),
        steps: Vec::new(),
    };
    let path = paths::run_actions(mur_home, name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, to_yaml(&run).unwrap()).unwrap();
}

#[test]
fn prune_keeps_newest_n_and_deletes_the_rest() {
    let temp = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", temp.path());
    for i in 0..10 {
        // Larger `i` -> further back in the past -> run-0 is newest.
        write_test_run(temp.path(), &format!("run-{i}"), i);
    }
    prune(3, None, false).unwrap();
    let mut remaining = directory_names(runs_dir().unwrap()).unwrap();
    remaining.sort();
    assert_eq!(remaining, vec!["run-0", "run-1", "run-2"]);
}

#[test]
fn prune_dry_run_deletes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", temp.path());
    for i in 0..10 {
        write_test_run(temp.path(), &format!("run-{i}"), i);
    }
    prune(3, None, true).unwrap();
    let remaining = directory_names(runs_dir().unwrap()).unwrap();
    assert_eq!(remaining.len(), 10, "dry_run must not delete anything");
}

#[test]
fn prune_keeps_unparsable_run_directories() {
    let temp = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", temp.path());
    for i in 0..5 {
        write_test_run(temp.path(), &format!("run-{i}"), i);
    }
    // A run directory whose actions.yaml is corrupt/missing must always survive.
    let bad_dir = paths::run_dir(temp.path(), "run-corrupt");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("actions.yaml"), "not: [valid yaml for a Run").unwrap();

    prune(1, None, false).unwrap();

    let remaining = directory_names(runs_dir().unwrap()).unwrap();
    assert!(
        remaining.contains(&"run-corrupt".to_string()),
        "unparsable run must be kept, got {remaining:?}"
    );
}

#[test]
fn prune_older_than_only_deletes_beyond_the_day_cutoff() {
    let temp = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", temp.path());
    // run-recent: 1 hour old. run-old: 40 days old.
    write_test_run(temp.path(), "run-recent", 60);
    write_test_run(temp.path(), "run-old", 40 * 24 * 60);

    // keep = 0 so both are candidates; older_than = 30 days should only
    // catch run-old.
    prune(0, Some(30), false).unwrap();

    let mut remaining = directory_names(runs_dir().unwrap()).unwrap();
    remaining.sort();
    assert_eq!(remaining, vec!["run-recent"]);
}
