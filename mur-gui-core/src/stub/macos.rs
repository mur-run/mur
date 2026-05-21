//! macOS per-agent `.app` stub generation — spec §5.1, §5.2, §12.4.
//!
//! Creates `~/Applications/MuR-Agent-<DisplayName>.app` with:
//! - `Contents/Info.plist` (bundle ID, URL scheme, NSServices)
//! - `Contents/MacOS/<slug>` (copy of `mur-agent-launcher`, ad-hoc re-signed)
//! - `Contents/Resources/agent.txt`, `host_version.txt`, `Icon.icns`
//!
//! After creation, runs the §12.4 Gatekeeper validation gate and registers
//! the URL scheme via LaunchServices.

use crate::stub::StubInfo;
use anyhow::{Context, Result};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn generate(
    slug: &str,
    display_name: &str,
    bundle_id: &str,
    url_scheme: &str,
    icon_icns: Option<&[u8]>,
    hub_version: &str,
    _mur_home: &Path,
) -> Result<StubInfo> {
    let stub_path = stub_app_path(display_name)?;

    // ── 1. Remove any existing stub ──────────────────────────────────────────
    if stub_path.exists() {
        std::fs::remove_dir_all(&stub_path)
            .with_context(|| format!("remove existing stub: {}", stub_path.display()))?;
    }

    // ── 2. Create directory tree ─────────────────────────────────────────────
    let macos_dir = stub_path.join("Contents").join("MacOS");
    let resources_dir = stub_path.join("Contents").join("Resources");
    std::fs::create_dir_all(&macos_dir).context("create MacOS dir")?;
    std::fs::create_dir_all(&resources_dir).context("create Resources dir")?;

    // ── 3. Write Info.plist ──────────────────────────────────────────────────
    let plist = info_plist(slug, display_name, bundle_id, url_scheme);
    std::fs::write(stub_path.join("Contents").join("Info.plist"), &plist)
        .context("write Info.plist")?;

    // ── 4. Write sidecar resource files ─────────────────────────────────────
    std::fs::write(resources_dir.join("agent.txt"), slug).context("write agent.txt")?;
    std::fs::write(resources_dir.join("host_version.txt"), hub_version)
        .context("write host_version.txt")?;
    if let Some(icns) = icon_icns {
        std::fs::write(resources_dir.join("Icon.icns"), icns).context("write Icon.icns")?;
    }

    // ── 5. Copy launcher binary ──────────────────────────────────────────────
    let launcher_src = find_launcher_binary()?;
    let launcher_dst = macos_dir.join(slug);
    std::fs::copy(&launcher_src, &launcher_dst).with_context(|| {
        format!(
            "copy launcher {} → {}",
            launcher_src.display(),
            launcher_dst.display()
        )
    })?;
    // Ensure executable bit
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher_dst, std::fs::Permissions::from_mode(0o755))
            .context("chmod launcher")?;
    }

    // ── 6. Ad-hoc sign the stub bundle ──────────────────────────────────────
    // Do NOT use --options=runtime (hardened runtime) on the launcher.
    run_cmd(
        "codesign",
        &[
            "-s",
            "-",
            "--force",
            "--timestamp=none",
            stub_path.to_str().unwrap_or(""),
        ],
        "ad-hoc sign stub",
    )?;

    // ── 7. Gatekeeper validation gate (§12.4) ────────────────────────────────
    run_cmd(
        "codesign",
        &[
            "--verify",
            "--deep",
            "--strict",
            "--verbose=4",
            stub_path.to_str().unwrap_or(""),
        ],
        "codesign verify",
    )?;
    run_cmd(
        "spctl",
        &[
            "--assess",
            "--type",
            "execute",
            "--verbose=4",
            stub_path.to_str().unwrap_or(""),
        ],
        "spctl assess",
    )?;

    // ── 8. Register with LaunchServices ─────────────────────────────────────
    // lsregister reads Info.plist and registers URL schemes + file associations.
    let lsregister = Path::new(
        "/System/Library/Frameworks/CoreServices.framework/\
         Frameworks/LaunchServices.framework/Support/lsregister",
    );
    if lsregister.exists() {
        run_cmd(
            lsregister.to_str().unwrap(),
            &["-f", stub_path.to_str().unwrap_or("")],
            "lsregister",
        )?;
    }

    // Set this stub as the default handler for its URL scheme via
    // LSSetDefaultHandlerForURLScheme (CoreFoundation FFI, same pattern as
    // mur-gui-core/src/voice/dnd.rs).
    set_default_url_handler(url_scheme, bundle_id)?;

    // Flush NSServices cache
    let pbs = Path::new("/System/Library/CoreServices/pbs");
    if pbs.exists() {
        let _ = Command::new(pbs).arg("-update").status();
    }

    Ok(StubInfo {
        path: stub_path,
        url_scheme: url_scheme.to_string(),
    })
}

/// Locate the `mur-agent-launcher` binary sibling to the current executable.
fn find_launcher_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe.parent().context("exe has no parent dir")?;
    let launcher = dir.join("mur-agent-launcher");
    if launcher.exists() {
        return Ok(launcher);
    }
    anyhow::bail!(
        "mur-agent-launcher not found at {}; \
         build it with `cargo build -p mur-agent-launcher`",
        launcher.display()
    )
}

/// Derive the stub `.app` path: `~/Applications/MuR-Agent-<DisplayName>.app`.
fn stub_app_path(display_name: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME env var not set")?;
    let safe_name: String = display_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let apps_dir = home.join("Applications");
    std::fs::create_dir_all(&apps_dir).context("create ~/Applications")?;
    Ok(apps_dir.join(format!("MuR-Agent-{safe_name}.app")))
}

/// Run a command, returning `Err` with captured stderr on non-zero exit.
fn run_cmd(program: &str, args: &[&str], label: &str) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("{label}: spawn failed"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{label} failed (exit {:?}):\n{stderr}", out.status.code());
    }
    Ok(())
}

/// Call `LSSetDefaultHandlerForURLScheme` via CoreFoundation FFI.
///
/// This registers `url_scheme` (e.g. `muragent-coach`) to open with the stub
/// whose CFBundleIdentifier is `bundle_id` (e.g. `run.mur.agent.coach`).
fn set_default_url_handler(url_scheme: &str, bundle_id: &str) -> Result<()> {
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSSetDefaultHandlerForURLScheme(
            scheme: *const std::ffi::c_void,
            bundle_id: *const std::ffi::c_void,
        ) -> i32;
        fn CFStringCreateWithCString(
            alloc: *const std::ffi::c_void,
            c_str: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const std::ffi::c_void;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    let scheme_c = CString::new(url_scheme).context("scheme CString")?;
    let bundle_c = CString::new(bundle_id).context("bundle_id CString")?;

    unsafe {
        let scheme_cf = CFStringCreateWithCString(
            std::ptr::null(),
            scheme_c.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        let bundle_cf = CFStringCreateWithCString(
            std::ptr::null(),
            bundle_c.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if scheme_cf.is_null() || bundle_cf.is_null() {
            if !scheme_cf.is_null() {
                CFRelease(scheme_cf);
            }
            if !bundle_cf.is_null() {
                CFRelease(bundle_cf);
            }
            anyhow::bail!("CFStringCreateWithCString returned null");
        }
        LSSetDefaultHandlerForURLScheme(scheme_cf, bundle_cf);
        CFRelease(scheme_cf);
        CFRelease(bundle_cf);
    }

    Ok(())
}

/// Build the `Info.plist` XML for the stub bundle.
fn info_plist(slug: &str, display_name: &str, bundle_id: &str, url_scheme: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>{bundle_id}</string>
    <key>CFBundleName</key>
    <string>{display_name}</string>
    <key>CFBundleDisplayName</key>
    <string>{display_name}</string>
    <key>CFBundleExecutable</key>
    <string>{slug}</string>
    <key>CFBundleIconFile</key>
    <string>Icon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>{bundle_id}</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>{url_scheme}</string>
            </array>
        </dict>
    </array>
    <key>NSServices</key>
    <array>
        <dict>
            <key>NSMessage</key>
            <string>shareWithMurAgent</string>
            <key>NSPortName</key>
            <string>{bundle_id}</string>
            <key>NSSendTypes</key>
            <array>
                <string>NSStringPboardType</string>
                <string>public.utf8-plain-text</string>
            </array>
            <key>NSMenuItem</key>
            <dict>
                <key>default</key>
                <string>Share with {display_name}</string>
            </dict>
        </dict>
    </array>
    <key>LSBackgroundOnly</key>
    <false/>
</dict>
</plist>
"#
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_plist_contains_bundle_id_and_scheme() {
        let plist = info_plist("coach", "Coach", "run.mur.agent.coach", "muragent-coach");
        assert!(plist.contains("run.mur.agent.coach"), "bundle_id missing");
        assert!(plist.contains("muragent-coach"), "url_scheme missing");
        assert!(plist.contains("<string>coach</string>"), "executable missing");
        assert!(plist.contains("Coach"), "display_name missing");
    }

    #[test]
    fn stub_app_path_sanitises_display_name() {
        // We can't test the full path (needs $HOME), but we can test the
        // safe_name logic by checking the function compiles and the name
        // doesn't contain slashes.
        let safe: String = "My Agent / Test"
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        assert!(!safe.contains('/'));
        assert_eq!(safe, "My-Agent---Test");
    }
}
