use super::*;
use std::time::Instant;

/// Beside the test binary, not `$TMPDIR`: some sandboxes deny exec there.
fn exec_dir() -> tempfile::TempDir {
    let exe = std::env::current_exe().unwrap();
    tempfile::tempdir_in(exe.parent().unwrap()).unwrap()
}

#[test]
fn probe_argv_is_isolated_and_reads_no_paths() {
    let v = probe_argv(Path::new("/m/sgconfig.yml"), "rs");
    let v: Vec<_> = v.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        v,
        [
            "run",
            "--config=/m/sgconfig.yml",
            "--pattern=x",
            "--lang=rs",
            "--stdin"
        ]
    );
}

#[cfg(unix)]
mod fake {
    use super::*;

    /// A fake ast-grep that appends one line per invocation to `calls`.
    pub struct Fake {
        pub dir: tempfile::TempDir,
        pub bin: PathBuf,
    }

    impl Fake {
        pub fn new(body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = exec_dir();
            let bin = dir.path().join("fake-ast-grep");
            let log = dir.path().join("calls");
            std::fs::write(
                &bin,
                format!("#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n", log.display()),
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir, bin }
        }

        pub fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        pub async fn probe(&self, lang: &str, t: Duration) -> Result<(), String> {
            let cfg = self.dir.path().join("sgconfig.yml");
            ensure_lang(&self.bin, self.dir.path(), &cfg, lang, t).await
        }
    }
}

#[cfg(unix)]
use fake::Fake;

#[cfg(unix)]
const T: Duration = Duration::from_secs(10);

#[cfg(unix)]
#[tokio::test]
async fn supported_is_cached_per_lang() {
    let f = Fake::new("exit 1");
    f.probe("rust", T).await.unwrap();
    f.probe("rust", T).await.unwrap();
    assert_eq!(f.calls().len(), 1, "second probe must hit the cache");
    f.probe("ts", T).await.unwrap();
    assert_eq!(f.calls().len(), 2, "a different lang is probed");
    assert!(f.calls()[0].contains("--stdin"), "{:?}", f.calls());
}

#[cfg(unix)]
#[tokio::test]
async fn unsupported_is_cached_and_quotes_stderr() {
    let f = Fake::new("echo 'klingon is not supported!' >&2; exit 2");
    for _ in 0..2 {
        let e = f.probe("klingon", T).await.unwrap_err();
        assert!(
            e.contains("unsupported lang 'klingon'") && e.contains("not supported!"),
            "{e}"
        );
    }
    assert_eq!(f.calls().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn indefinite_exit_is_not_cached() {
    let f = Fake::new("echo boom >&2; exit 3");
    for _ in 0..2 {
        let e = f.probe("rust", T).await.unwrap_err();
        assert!(e.contains("exited 3") && e.contains("boom"), "{e}");
    }
    assert_eq!(f.calls().len(), 2, "a flaky probe must be retried");
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_is_bounded_and_not_cached() {
    // Hangs while the test-owned `slow` marker exists, answers once it is gone.
    let f = Fake::new("[ -e \"$(dirname \"$0\")/slow\" ] && exec sleep 30; exit 1");
    let slow = f.dir.path().join("slow");
    std::fs::write(&slow, b"").unwrap();
    let t = Instant::now();
    let e = f
        .probe("rust", Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert!(e.contains("timed out"), "{e}");
    std::fs::remove_file(&slow).unwrap();
    // Not cached: the same lang is probed again and now answers.
    f.probe("rust", T).await.unwrap();
}

/// The pinned binary, if the env var names one: aliases are accepted.
#[tokio::test]
async fn live_probe_accepts_aliases_and_rejects_unknown() {
    let Some(bin) = std::env::var_os("MUR_AST_GREP_BIN").map(PathBuf::from) else {
        eprintln!("skip: MUR_AST_GREP_BIN not set; ast-grep lang probe not checked");
        return;
    };
    let d = exec_dir();
    let cfg = d.path().join("sgconfig.yml");
    std::fs::write(&cfg, b"").unwrap();
    for l in ["rust", "rs", "Rust", "typescript", "ts", "python"] {
        ensure_lang(&bin, d.path(), &cfg, l, PROBE_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("{l}: {e}"));
    }
    let e = ensure_lang(&bin, d.path(), &cfg, "klingon", PROBE_TIMEOUT)
        .await
        .unwrap_err();
    assert!(e.contains("unsupported lang"), "{e}");
}
