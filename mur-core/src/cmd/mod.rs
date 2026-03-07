pub(crate) mod community_cmd;
pub(crate) mod context;
pub(crate) mod evolve_cmd;
pub(crate) mod init;
pub(crate) mod inject_cmd;
pub(crate) mod learn;
pub(crate) mod misc;
pub(crate) mod pattern;
pub(crate) mod reindex;
pub(crate) mod server_cmd;
pub(crate) mod session;
pub(crate) mod sync_cmd;
pub(crate) mod workflow;

pub(crate) fn read_multiline() -> anyhow::Result<String> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        lines.push(line);
    }
    Ok(lines.join("").trim_end().to_string())
}
