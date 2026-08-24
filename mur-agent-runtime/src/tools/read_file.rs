//! First-party read-only file tool (issue #591, PR1).
//!
//! Complements the OS sandbox with an in-process entitlement check so a
//! disallowed read fails with a clear, policy-shaped error instead of a
//! kernel denial, and so file access is visible to per-tool policy instead
//! of hiding inside `bash`.

use std::path::Path;

use mur_common::agent::FilesystemEntitlement;

use crate::llm::ToolDef;
use crate::tools::{ToolError, ToolExecutor, ToolOutput};

/// The image media type for a path's extension, or `None` for everything else.
///
/// Deliberately narrower than the runtime's general extension→MIME map: this
/// gates what gets handed to a vision model, so it lists only types a provider
/// accepts. `image/svg+xml` is absent on purpose — it is markup, and reading it
/// as text is the more useful answer.
fn image_media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Hard ceiling on returned bytes so a huge file cannot blow up the turn.
const MAX_RETURN_BYTES: usize = 512 * 1024;

pub struct ReadFileTool {
    /// Session cwd shared with `bash`; relative paths resolve against its
    /// current snapshot so `read_file rel/x` matches where `bash` last ran.
    pub session_cwd: crate::tools::fs_policy::SessionCwd,
    /// Filesystem grants from the agent profile; checked before every read.
    pub fs: FilesystemEntitlement,
    /// MUR's own launch chain. Checked before `fs`, and no grant can satisfy it.
    pub chain: crate::sandbox::launch_chain::LaunchChain,
}

impl ReadFileTool {
    pub fn new(
        session_cwd: crate::tools::fs_policy::SessionCwd,
        fs: FilesystemEntitlement,
        chain: crate::sandbox::launch_chain::LaunchChain,
    ) -> Self {
        Self {
            session_cwd,
            fs,
            chain,
        }
    }

    /// Test-only: construct with an inert launch chain. Production must go
    /// through `new`, which cannot be called without a real chain.
    #[cfg(test)]
    pub fn new_for_test(
        session_cwd: crate::tools::fs_policy::SessionCwd,
        fs: FilesystemEntitlement,
    ) -> Self {
        Self::new(
            session_cwd,
            fs,
            crate::sandbox::launch_chain::LaunchChain::inert(),
        )
    }

    /// Prefix-match `path` against the entitlement lists after
    /// canonicalization. `deny` always wins; a read is allowed when the path
    /// falls under any `read` OR `write` grant (write implies read-back).
    fn check_entitlement(&self, canonical: &Path) -> Result<(), ToolError> {
        // Before the lists, and unconditionally: reading another agent's
        // signing key is enough to forge its signed events, and no entitlement
        // may authorise that.
        if let Some(reason) = self.chain.protects_read(canonical) {
            return Err(ToolError::Execution(format!(
                "path is part of MUR's launch chain and can never be read: {} ({reason})",
                canonical.display()
            )));
        }
        if crate::tools::fs_policy::under_any(&self.fs.deny, canonical) {
            return Err(ToolError::Execution(format!(
                "path denied by entitlement: {}",
                canonical.display()
            )));
        }
        if crate::tools::fs_policy::under_any(&self.fs.read, canonical)
            || crate::tools::fs_policy::under_any(&self.fs.write, canonical)
        {
            return Ok(());
        }
        Err(ToolError::Execution(format!(
            "path not entitled: {} (grant it via `mur agent perm allow-read`)",
            canonical.display()
        )))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".into(),
            description: "Read a UTF-8 text file, or an image you want to LOOK AT. \
PNG/JPEG/GIF/WebP are returned as real image input you can see — use this rather than describing a picture you have not been shown. Optional 1-indexed `offset`/`limit` select a line window (text only). \
Relative paths resolve against the shared session working directory (the same base the `bash` tool uses, moved only by passing `bash` an explicit `cwd`); reads are checked against the agent's filesystem entitlements."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": format!("File to read ({})", crate::tools::fs_policy::PATH_FORMS) },
                    "offset": { "type": "integer", "description": "1-indexed first line to return" },
                    "limit": { "type": "integer", "description": "Maximum number of lines to return" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let raw = input["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".into()))?;
        let base = self.session_cwd.current();
        let joined = crate::tools::fs_policy::resolve_path(&base, raw);
        let canonical = std::fs::canonicalize(&joined).map_err(|e| {
            ToolError::Execution(crate::tools::fs_policy::format_io_error(
                "read", &joined, &base, &e,
            ))
        })?;
        self.check_entitlement(&canonical)?;

        let bytes = std::fs::read(&canonical).map_err(|e| {
            ToolError::Execution(crate::tools::fs_policy::format_io_error(
                "read", &canonical, &base, &e,
            ))
        })?;
        // An image is returned AS an image, not as its bytes decoded into
        // mojibake. Before this branch existed a vision-capable agent that
        // fetched a photo could only read it as text and then guess at what
        // it "saw" — the model has no way to signal that it never got a
        // picture, so the guess came back with a confidence score attached.
        if let Some(media_type) = image_media_type(&canonical) {
            if bytes.len() > crate::tools::MAX_IMAGE_BYTES {
                return Err(ToolError::Execution(format!(
                    "image is {} bytes, over the {} byte limit a vision request accepts: {}. \
                     Resize it before reading, or read it with `bash` if you only need the bytes.",
                    bytes.len(),
                    crate::tools::MAX_IMAGE_BYTES,
                    canonical.display()
                )));
            }
            use base64::Engine as _;
            return Ok(ToolOutput {
                text: format!(
                    "[image {} — {}, {} bytes]",
                    canonical.display(),
                    media_type,
                    bytes.len()
                ),
                status: crate::tools::ToolStatus::Ok,
                images: vec![crate::tools::ToolImage {
                    media_type: media_type.to_string(),
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                }],
            });
        }

        let text = String::from_utf8_lossy(&bytes);

        let offset = input["offset"].as_i64().filter(|v| *v >= 1);
        let limit = input["limit"].as_i64().filter(|v| *v >= 1);
        let windowed: String = match (offset, limit) {
            (None, None) => text.into_owned(),
            (o, l) => {
                let start = o.unwrap_or(1) as usize - 1;
                let take = l.map(|v| v as usize).unwrap_or(usize::MAX);
                text.lines()
                    .skip(start)
                    .take(take)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        if windowed.len() > MAX_RETURN_BYTES {
            let mut cut = windowed.into_bytes();
            cut.truncate(MAX_RETURN_BYTES);
            let mut s = String::from_utf8_lossy(&cut).into_owned();
            s.push_str("\n… [truncated at 512KiB — use offset/limit to window]");
            return Ok(s.into());
        }
        Ok(windowed.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fs_ent(read: &[&str], write: &[&str], deny: &[&str]) -> FilesystemEntitlement {
        FilesystemEntitlement {
            read: read.iter().map(|s| s.to_string()).collect(),
            write: write.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A 1x1 PNG read through the tool must come back as image input, not as
    /// its bytes lossily decoded into text — the exact failure that made a
    /// vision-capable agent invent a car model it had never been shown.
    #[test]
    fn image_file_returns_image_input_not_mojibake() {
        use base64::Engine as _;
        let tmp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        // Smallest valid PNG header bytes; content does not matter here,
        // only that the tool routes on the extension and preserves bytes.
        let png: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let path = home.join("shot.png");
        std::fs::write(&path, png).unwrap();

        let tool = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(home.clone()),
            fs_ent(&[&home.to_string_lossy()], &[], &[]),
        );
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(serde_json::json!({"path": path.to_str().unwrap()})))
            .expect("read must succeed");

        assert_eq!(out.images.len(), 1, "the png must arrive as image input");
        assert_eq!(out.images[0].media_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&out.images[0].data)
                .unwrap(),
            png,
            "bytes must survive the round trip intact"
        );

        // Negative control: a text file under the same grant still returns
        // text and carries no images, so the branch above is the extension
        // routing and not "everything is an image now".
        let txt = home.join("notes.txt");
        std::fs::write(&txt, "hello").unwrap();
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(serde_json::json!({"path": txt.to_str().unwrap()})))
            .expect("read must succeed");
        assert!(out.images.is_empty(), "text file must not produce images");
        assert_eq!(out.text, "hello");
    }

    #[test]
    fn sibling_identity_key_is_refused_even_under_a_read_grant() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical base: the check compares canonicalized paths, and on macOS
        // /var is a symlink to /private/var — raw tempdir paths would never
        // match the canonicalized grant roots the check computes.
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        let agents = home.join("agents");
        std::fs::create_dir_all(agents.join("pm")).unwrap();
        std::fs::write(agents.join("pm/identity.key"), b"SECRET").unwrap();
        std::fs::write(agents.join("pm/profile.yaml"), b"name: pm\n").unwrap();

        let chain = crate::sandbox::launch_chain::LaunchChain::for_test(
            &agents.join("mur"),
            &home.join("bin"),
            &home.join("home"),
        );
        let fs = fs_ent(&[&home.to_string_lossy()], &[], &[]);
        let tool = ReadFileTool::new(
            crate::tools::fs_policy::SessionCwd::new(home.clone()),
            fs,
            chain,
        );

        let err = tool
            .check_entitlement(&agents.join("pm/identity.key"))
            .expect_err("a sibling signing key must be refused under any read grant");
        assert!(format!("{err:?}").contains("forge"), "error must say why");

        // Negative control: the same grant still reads a neighbouring file, so
        // the refusal is the key rule and not a broken read path.
        tool.check_entitlement(&agents.join("pm/profile.yaml"))
            .expect("sibling profile.yaml is not read-protected");
    }

    fn write_tmp(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    // runtime-file-tools-cwd: a shared SessionCwd handle means the bash tool's
    // explicit `cwd` moves the base that the file tools resolve against.
    #[tokio::test]
    async fn shared_cwd_bash_set_moves_read_file_base() {
        use crate::tools::bash::BashTool;
        use crate::tools::fs_policy::SessionCwd;

        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        // Seed the SAME relative filename in both dirs with distinct contents.
        write_tmp(home.path(), "spec.md", "HOME");
        write_tmp(other.path(), "spec.md", "OTHER");

        let shared = SessionCwd::new(home.path().into());
        let bash = BashTool::new(home.path().into(), shared.clone());
        let reader = ReadFileTool::new_for_test(
            shared.clone(),
            fs_ent(
                &[
                    home.path().to_str().unwrap(),
                    other.path().to_str().unwrap(),
                ],
                &[],
                &[],
            ),
        );

        // Before: relative read resolves against home.
        let before = reader
            .execute(serde_json::json!({"path": "spec.md"}))
            .await
            .unwrap();
        assert!(
            before.text.contains("HOME"),
            "expected HOME, got {}",
            before.text
        );

        // bash with explicit cwd moves the shared base to `other`.
        bash.execute(serde_json::json!({"command": "true", "cwd": other.path().to_str().unwrap()}))
            .await
            .unwrap();

        // After: the SAME relative read now resolves against `other`.
        let after = reader
            .execute(serde_json::json!({"path": "spec.md"}))
            .await
            .unwrap();
        assert!(
            after.text.contains("OTHER"),
            "expected OTHER after bash cwd, got {}",
            after.text
        );
    }

    #[tokio::test]
    async fn missing_file_error_names_session_cwd() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[]),
        );
        let err = t
            .execute(serde_json::json!({"path": "nope.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("relative to session cwd"));
    }

    #[tokio::test]
    async fn missing_path_is_invalid_input() {
        let td = tempfile::tempdir().unwrap();
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[], &[], &[]),
        );
        let err = t.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn entitled_read_with_window() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "l1\nl2\nl3\nl4");
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[]),
        );
        let out = t
            .execute(serde_json::json!({"path": "f.txt", "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert_eq!(out.text, "l2\nl3");
    }

    #[tokio::test]
    async fn write_grant_implies_read() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "hi");
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[], &[root], &[]),
        );
        assert_eq!(
            t.execute(serde_json::json!({"path": "f.txt"}))
                .await
                .unwrap()
                .text,
            "hi"
        );
    }

    #[tokio::test]
    async fn deny_wins_over_read_grant() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "secret");
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[root]),
        );
        let err = t
            .execute(serde_json::json!({"path": "f.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("denied"));
    }

    #[tokio::test]
    async fn unentitled_path_is_rejected() {
        let td = tempfile::tempdir().unwrap();
        write_tmp(td.path(), "f.txt", "hi");
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&["/nonexistent-grant"], &[], &[]),
        );
        let err = t
            .execute(serde_json::json!({"path": "f.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not entitled"));
    }

    #[tokio::test]
    async fn nonexistent_file_is_execution_error() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_str().unwrap();
        let t = ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(td.path().into()),
            fs_ent(&[root], &[], &[]),
        );
        let err = t
            .execute(serde_json::json!({"path": "nope.txt"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)));
    }
}
