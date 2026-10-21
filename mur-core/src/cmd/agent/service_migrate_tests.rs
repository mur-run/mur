//! The descriptors these tests call "pre-#2015" are copied verbatim from the
//! templates `install-service` wrote at 69b27fba — the ones still loaded on
//! every machine that installed a service before the fix.

use std::path::Path;

use super::has_refusal_guard;

const MARKER: &str = "/m/agents/kelp/start_refused.json";

#[cfg(target_os = "macos")]
const PRE_2015: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>run.mur.agent.kelp</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/mur_agent_kelp</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/bin</string>
    </dict>
    <key>StandardErrorPath</key>
    <string>/tmp/mur-agent-kelp.err.log</string>
    <key>StandardOutPath</key>
    <string>/tmp/mur-agent-kelp.out.log</string>
</dict>
</plist>
"#;

#[cfg(not(target_os = "macos"))]
const PRE_2015: &str = r#"[Unit]
Description=murmur agent kelp
After=default.target

[Service]
Type=simple
Environment=PATH=/usr/bin
ExecStart=/bin/mur_agent_kelp start
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
"#;

fn current() -> String {
    #[cfg(target_os = "macos")]
    {
        super::super::service::darwin_plist(
            "kelp",
            Path::new("/bin/mur_agent_kelp"),
            "/usr/bin",
            Path::new(MARKER),
        )
    }
    #[cfg(target_os = "linux")]
    {
        super::super::service::linux_unit("kelp", Path::new("/bin/mur_agent_kelp"), "/usr/bin")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        format!(
            "RestartPreventExitStatus={}\n",
            mur_common::start_refusal::EXIT_CODE
        )
    }
}

#[test]
fn a_pre_2015_descriptor_needs_migrating() {
    assert!(
        !has_refusal_guard(PRE_2015, Path::new(MARKER)),
        "the descriptor that respawns forever must be detected"
    );
}

#[test]
fn the_current_template_is_already_guarded() {
    // Otherwise `start` would re-install (and reload) on every run.
    assert!(has_refusal_guard(&current(), Path::new(MARKER)));
}

/// A plist gated on a different agent's marker is not this agent's guard —
/// e.g. a descriptor copied between agents by hand.
#[cfg(target_os = "macos")]
#[test]
fn a_guard_on_another_marker_does_not_count() {
    let other = current().replace(MARKER, "/m/agents/other/start_refused.json");
    assert!(!has_refusal_guard(&other, Path::new(MARKER)));
}

#[test]
fn an_agent_without_a_service_never_needs_migrating() {
    let tmp = tempfile::tempdir().unwrap();
    // No descriptor is installed under this name on any test machine.
    assert!(!super::needs_migration(
        "no_such_agent_2015_migration_probe",
        tmp.path()
    ));
}
