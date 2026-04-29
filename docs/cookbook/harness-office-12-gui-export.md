# Cookbook — `OFFICE-12-GUI-EXPORT` harness scenario

This is a Python-ready scenario specification for the `mur-agent-harness/` repo's next phase. It complements the existing `OFFICE-1` … `OFFICE-11` scenarios with end-to-end coverage of `mur agent export --format gui` (PR [mur-run/mur#41](https://github.com/mur-run/mur/pull/41)).

The same logic is already exercised in pure bash via `scripts/e2e/p1-export-gui.sh`; this doc lets the harness author re-implement it in the harness's idiomatic style (heartbeat + JSON line log + REPORT.md output).

## Scenario contract

```yaml
id: OFFICE-12-GUI-EXPORT
prereqs:
  - macOS host (linux + windows variants are P2)
  - HARNESS_MUR_BIN points at a mur build from PR #41+
  - tauri-cli installed (`mur agent doctor --format gui` returns ok)
  - npm + node 20 installed
agents:
  - p12-orchestrator (echo mode, accepts_from=["*"])
exit_criteria:
  - bundle.app produced under dist/
  - Info.plist CFBundleIdentifier == "run.mur.agent.<safe-name>"
  - Contents/MacOS/{mur-agent-gui, mur-agent-runtime} both present
  - Contents/Resources/{agent-payload.tar.gz, metadata.json, themes/} present
  - Bundle launches → bootstraps payload to ~/.mur/agents/<name>/
  - running.lock appears in agent_home within 30s
  - Bundle exits cleanly on SIGTERM (no orphan running.lock)
```

## Step-by-step

```python
def office_12_gui_export(h: Harness) -> bool:
    """End-to-end test of mur agent export --format gui."""

    # 1. Pre-flight: doctor must report ok for gui.
    out = run_mur("agent", "doctor", "--format", "gui", "--json")
    h.checkpoint("OFFICE-12.doctor", json=out.stdout)
    checks = json.loads(out.stdout)
    missing = [r for r in checks if r["status"] == "missing"]
    if missing:
        h.record_error("OFFICE-12.doctor_missing", checks=missing)
        return False

    # 2. Provision a throwaway agent in a sandbox MUR_HOME.
    sandbox = Path(tempfile.mkdtemp(prefix="office-12-"))
    env = {**os.environ, "MUR_HOME": str(sandbox), "MUR_AGENT_BIN_DIR": str(sandbox / "bin")}

    h.ensure_profile(AgentSpec("p12-orchestrator"), env=env)
    h.checkpoint("OFFICE-12.created")

    # 3. Run the export pipeline.
    out_path = sandbox / "MyAgent.app"
    res = subprocess.run(
        [MUR, "agent", "export", "p12-orchestrator", "-o", str(out_path),
         "--format", "gui", "--theme", "dark", "--skip-notarize"],
        env=env, capture_output=True, text=True, timeout=600,
    )
    if res.returncode != 0:
        h.record_error("OFFICE-12.export_failed", stdout=res.stdout, stderr=res.stderr)
        return False
    h.checkpoint("OFFICE-12.exported", elapsed_secs=...)

    # 4. Bundle structure assertions.
    assert (out_path / "Contents" / "MacOS" / "mur-agent-gui").is_file()
    assert (out_path / "Contents" / "MacOS" / "mur-agent-runtime").is_file()
    for r in ["agent-payload.tar.gz", "metadata.json", "themes"]:
        assert (out_path / "Contents" / "Resources" / r).exists(), f"missing resource {r}"

    # 5. Info.plist values.
    info_plist = (out_path / "Contents" / "Info.plist").read_text()
    assert "run.mur.agent.p12-orchestrator" in info_plist
    h.checkpoint("OFFICE-12.bundle_structure_ok")

    # 6. macOS App Translocation workaround — copy to a non-symlinked
    #    path before launching. /var/folders is symlinked → /private/var,
    #    which Tauri rejects in current_exe().
    stable = Path.home() / ".cache" / "mur" / "office-12-test"
    if stable.exists():
        shutil.rmtree(stable)
    stable.mkdir(parents=True)
    stable_app = stable / out_path.name
    subprocess.check_call(["cp", "-R", str(out_path), str(stable_app)])

    # 7. Launch + wait for running.lock.
    exe = stable_app / "Contents" / "MacOS" / "mur-agent-gui"
    proc = subprocess.Popen([str(exe)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    lock = sandbox / "agents" / "p12-orchestrator" / "running.lock"
    deadline = time.time() + 30
    while time.time() < deadline and not lock.exists():
        time.sleep(0.5)
    if not lock.exists():
        proc.terminate()
        h.record_error("OFFICE-12.lock_timeout", deadline_s=30)
        return False

    # 8. Validate the lock card has expected shape.
    card = json.loads(lock.read_text())
    for k in ["uuid", "name", "pid", "started_at", "transports", "capabilities"]:
        assert k in card, f"running.lock missing field: {k}"
    assert card["name"] == "p12-orchestrator"
    h.checkpoint("OFFICE-12.spawned", lock=card)

    # 9. SIGTERM → clean shutdown.
    proc.terminate()
    proc.wait(timeout=10)
    time.sleep(2)
    if lock.exists():
        h.record_error("OFFICE-12.dirty_shutdown", lock_still_present=True)
        return False

    h.checkpoint("OFFICE-12.complete")
    return True
```

## Notes for the harness author

- Add `OFFICE-12-GUI-EXPORT` to the `phaseN/design.md` list in `mur-agent-harness/`.
- The 600s timeout in step 3 covers cold cargo build + npm install on a clean cache. Warm runs typically finish under 90s.
- The harness's own `~/.mur/agents/` is reused by default; this scenario isolates via a sandbox `MUR_HOME` so it doesn't pollute the host's real state.
- Don't try to interact with the GUI itself (tabs / tray) from Python — that's outside the harness's competence. The lock file is the contract; everything past launch is verified by other layers (`scripts/e2e/p1-export-gui.sh`).
- Heartbeat semantics from existing scenarios: poll the lock file, not the GUI process. The GUI can be SIGCONT'd / suspended without the lock file disappearing.

## Related

- Bash equivalent: `scripts/e2e/p1-export-gui.sh` (runs the same flow, in-tree)
- Spec: `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md`
- Plan: `docs/superpowers/plans/2026-04-29-mur-agent-gui-export-plan-COMPLETE.md`
