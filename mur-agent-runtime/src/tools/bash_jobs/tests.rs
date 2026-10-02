use super::*;

fn spec<'a>(cmd: &'a str, dir: &'a Path, vault: Option<Arc<SecretVault>>) -> SpawnSpec<'a> {
    SpawnSpec {
        command: cmd,
        cwd: dir,
        env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
            .into_iter()
            .chain(vault.iter().flat_map(|v| v.env_pairs()))
            .collect(),
        spool_dir: dir,
        vault,
    }
}

#[cfg(unix)]
async fn wait_dead(pid: u32, within: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if !pid_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !pid_alive(pid)
}

/// Test 1 — the assertion that separates yield from kill: after the wait
/// runs out the pid is still alive.
#[cfg(unix)]
#[tokio::test]
async fn a_timed_out_wait_returns_running_and_leaves_the_child_alive() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    let id = t.spawn(spec("sleep 3", dir.path(), None)).unwrap();
    let t0 = Instant::now();
    let p = t.poll(&id, Duration::from_secs(1)).await.unwrap();
    assert!(p.exit.is_none(), "{p:?}");
    assert!(
        t0.elapsed() < Duration::from_millis(2500),
        "waited the whole sleep"
    );
    assert!(pid_alive(t.pid(&id).unwrap()), "the clock killed the child");
    assert_eq!(t.running_ids(), vec![id.clone()]);
    // Test 2 — a later wait collects the exit and the job is gone.
    let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
    assert_eq!(p.exit.as_ref().and_then(|e| e.code), Some(0), "{p:?}");
    assert!(matches!(
        t.poll(&id, Duration::ZERO).await,
        Err(JobError::Unknown(..))
    ));
}

/// Test 5 — zero wait is the background case.
#[cfg(unix)]
#[tokio::test]
async fn zero_wait_returns_at_once_before_any_output() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    let id = t
        .spawn(spec("sleep 0.5; echo late", dir.path(), None))
        .unwrap();
    let p = t.poll(&id, Duration::ZERO).await.unwrap();
    assert!(p.exit.is_none());
    assert_eq!(p.new_stdout, "");
    t.kill_all().await;
}

/// Test 6 — output after the yield arrives on the next wait, exactly once.
#[cfg(unix)]
#[tokio::test]
async fn output_after_a_yield_is_delivered_once() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    let id = t
        .spawn(spec("echo a; sleep 1; echo b", dir.path(), None))
        .unwrap();
    let p1 = t.poll(&id, Duration::from_millis(300)).await.unwrap();
    assert_eq!(p1.new_stdout, "a\n", "{p1:?}");
    assert!(p1.exit.is_none());
    let p2 = t.poll(&id, Duration::from_secs(5)).await.unwrap();
    assert_eq!(p2.new_stdout, "b\n", "{p2:?}");
    assert_eq!(p2.exit.as_ref().and_then(|e| e.code), Some(0));
    assert!(p2.bytes_seen > p1.bytes_seen);
}

/// Test 3 + 4 — D9: killing the job kills the grandchild too, and the
/// shell is reaped (no zombie), which is what `kill_on_drop` alone used to
/// get wrong.
#[cfg(unix)]
#[tokio::test]
async fn kill_ends_the_whole_group_and_reaps_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    let id = t
        .spawn(spec("sleep 60 & echo $!; wait", dir.path(), None))
        .unwrap();
    let p = t.poll(&id, Duration::from_millis(500)).await.unwrap();
    let grandchild: u32 = p
        .new_stdout
        .trim()
        .parse()
        .expect("grandchild pid on stdout");
    assert!(pid_alive(grandchild));
    let shell = t.pid(&id).unwrap();
    let k = t.kill(&id).await.unwrap();
    assert!(k.exit.as_ref().and_then(|e| e.killed_by).is_some(), "{k:?}");
    assert!(
        wait_dead(grandchild, KILL_GRACE + Duration::from_secs(1)).await,
        "grandchild {grandchild} outlived bash_kill"
    );
    // Reaped by the pump's `wait`, so the kernel has no child entry left.
    let mut status: libc::c_int = 0;
    let ret = unsafe { libc::waitpid(shell as libc::pid_t, &mut status, libc::WNOHANG) };
    assert_eq!(ret, -1, "shell {shell} still a child (zombie or running)");
}

/// Test 8 — the cap names the running jobs.
#[cfg(unix)]
#[tokio::test]
async fn the_ninth_job_is_refused_and_names_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    for _ in 0..MAX_JOBS {
        t.spawn(spec("sleep 30", dir.path(), None)).unwrap();
    }
    match t.spawn(spec("true", dir.path(), None)) {
        Err(JobError::TooMany(list)) => assert_eq!(list.matches("j-").count(), MAX_JOBS),
        other => panic!("expected TooMany, got {other:?}"),
    }
    assert_eq!(t.kill_all().await, MAX_JOBS);
}

/// Test 10 — D10 end to end: the secret reaches the child, comes back in
/// two pipe writes, and the spool FILE holds the mask, not the value.
#[cfg(unix)]
#[tokio::test]
async fn spool_holds_the_mask_even_when_the_secret_arrives_in_two_reads() {
    let dir = tempfile::tempdir().unwrap();
    let v = Arc::new(SecretVault::new());
    v.set("TOK", "hunter2hunter2").unwrap();
    let t = JobTable::new();
    let id = t
        .spawn(spec(
            "printf '%s' \"${TOK:0:3}\"; sleep 0.3; printf '%s\\n' \"${TOK:3}\"",
            dir.path(),
            Some(v),
        ))
        .unwrap();
    let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
    assert_eq!(p.new_stdout, "[SECRET:TOK]\n", "{p:?}");
    let spool = std::fs::read_to_string(p.spool.unwrap()).unwrap();
    assert_eq!(spool, "[SECRET:TOK]\n");
}

/// Test 11 — D8: ownership is the task-local at spawn; cleanup is per
/// owner; a job spawned outside any scope is only `kill_all`'s.
#[cfg(unix)]
#[tokio::test]
async fn kill_owned_by_kills_only_that_tasks_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    let a = CURRENT_TASK_ID
        .scope("task-A".to_string(), async {
            t.spawn(spec("sleep 30", dir.path(), None))
        })
        .await
        .unwrap();
    let b = CURRENT_TASK_ID
        .scope("task-B".to_string(), async {
            t.spawn(spec("sleep 30", dir.path(), None))
        })
        .await
        .unwrap();
    let orphan = t.spawn(spec("sleep 30", dir.path(), None)).unwrap();
    // Captured before killing: `kill` removes an exited job from the
    // table (§4, "reported once"), so `pid(&a)` after `kill_owned_by`
    // would return `None` — and `pid_alive(0)` signals this process's
    // own group via `kill(0, 0)`, which succeeds, so the bug hides as a
    // false pass rather than a panic.
    let pid_a = t.pid(&a).unwrap();
    let pid_b = t.pid(&b).unwrap();
    let pid_orphan = t.pid(&orphan).unwrap();
    assert_eq!(t.kill_owned_by("task-nope").await, 0);
    assert_eq!(t.kill_owned_by("task-A").await, 1);
    assert!(wait_dead(pid_a, KILL_GRACE + Duration::from_secs(1)).await);
    assert!(pid_alive(pid_b), "B's job died with A");
    assert!(pid_alive(pid_orphan));
    assert_eq!(t.kill_all().await, 2);
}

/// Test 7 (tail half) — more output than the tail keeps is reported as
/// skipped, and the reply is exactly the tail.
#[cfg(unix)]
#[tokio::test]
async fn tail_window_reports_what_it_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let t = JobTable::new();
    let id = t
        .spawn(spec(
            "head -c 40000 /dev/zero | tr '\\0' x",
            dir.path(),
            None,
        ))
        .unwrap();
    let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
    assert_eq!(p.new_stdout.len(), TAIL_BYTES);
    assert_eq!(p.skipped, 40000 - TAIL_BYTES as u64);
    assert_eq!(p.bytes_seen, 40000);
}

/// Test 7 (spool half) — past the cap the spool stops and says so; the
/// tail keeps everything.
#[test]
fn spool_stops_at_the_cap_and_the_tail_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cap.log");
    let file = Mutex::new(std::fs::File::create(&path).unwrap());
    let out = Mutex::new(Output::default());
    append(&out, Which::Stdout, Some(&file), b"0123456789", 10);
    append(&out, Which::Stdout, Some(&file), b"abcdef", 10);
    let o = out.lock().unwrap();
    assert!(o.spool_capped);
    assert_eq!(o.spool_written, 10);
    assert_eq!(o.stdout.total, 16);
    drop(o);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "0123456789");
}

/// A spool that cannot be written is a note, not a dead job.
#[cfg(unix)]
#[tokio::test]
async fn unwritable_spool_dir_is_a_note_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("not-a-dir");
    std::fs::write(&bad, b"file").unwrap();
    let t = JobTable::new();
    let s = SpawnSpec {
        command: "echo ok",
        cwd: dir.path(),
        env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
        spool_dir: &bad,
        vault: None,
    };
    let id = t.spawn(s).unwrap();
    let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
    assert_eq!(p.new_stdout, "ok\n");
    assert!(p.spool.is_none());
    assert!(
        p.spool_note
            .as_deref()
            .unwrap_or("")
            .starts_with("spool unavailable"),
        "{p:?}"
    );
}
