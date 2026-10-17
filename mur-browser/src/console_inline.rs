//! Inline the console log that `@playwright/mcp` parks in a file (#1685
//! follow-up).
//!
//! When a page logs to the console, every action response carries
//! `- New console entries: <file>#L<a>-L<b>` instead of the entries
//! themselves, and 0.0.82 has no option to inline them (`--console-level`
//! only filters). A live agent is sealed, so following that link needs the
//! built-in `read_file`, whose approval a non-interactive caller cannot give.
//! This hook reads the referenced lines itself and splices them in, so the
//! agent never sees a file link.
//!
//! Only files inside the private output directory are read: the link comes
//! from the page-controlled server output, and this must not become a way to
//! read arbitrary host files into the agent's context.

use crate::proxy::{Decision, Downstream, Hook, Request};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The line prefix `@playwright/mcp` 0.0.82 writes for a console log link.
pub const CONSOLE_LINK_PREFIX: &str = "- New console entries: ";
/// Most console lines spliced into one response. A chatty page must not blow
/// the agent's context; the remainder is summarised as a count.
pub const MAX_INLINE_LINES: usize = 50;
/// Longest single console line kept, in characters.
pub const MAX_LINE_CHARS: usize = 500;

/// Wraps another hook; inlines console links when `root` is set.
pub struct ConsoleInlineHook<H> {
    inner: H,
    root: Option<PathBuf>,
}

impl<H> ConsoleInlineHook<H> {
    /// `root` is the `--output-dir` the server writes logs into; `None`
    /// leaves every response untouched (the recording modes).
    pub fn new(inner: H, root: Option<PathBuf>) -> Self {
        Self { inner, root }
    }
}

impl<H: Hook> Hook for ConsoleInlineHook<H> {
    async fn on_request(&mut self, req: Request, down: &Downstream) -> Decision {
        self.inner.on_request(req, down).await
    }

    async fn on_response(&mut self, req: &Request, resp: Value, down: &Downstream) -> Value {
        let mut resp = self.inner.on_response(req, resp, down).await;
        if let Some(root) = &self.root {
            inline_console_links(&mut resp, root);
        }
        resp
    }

    fn extra_tools(&self) -> Vec<Value> {
        self.inner.extra_tools()
    }
}

/// Rewrite every text block of a `tools/call` result in place.
pub fn inline_console_links(resp: &mut Value, root: &Path) {
    let Some(content) = resp
        .get_mut("result")
        .and_then(|r| r.get_mut("content"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            continue;
        };
        if !text.contains(CONSOLE_LINK_PREFIX) {
            continue;
        }
        let rewritten = rewrite_text(text, root);
        block["text"] = Value::String(rewritten);
    }
}

fn rewrite_text(text: &str, root: &Path) -> String {
    let mut out = Vec::new();
    for line in text.split('\n') {
        match line.strip_prefix(CONSOLE_LINK_PREFIX) {
            Some(link) => out.push(render_link(link, root)),
            None => out.push(line.to_owned()),
        }
    }
    out.join("\n")
}

/// The replacement for one link line. Never echoes the path back: a link the
/// agent cannot open is the failure this module exists to remove.
fn render_link(link: &str, root: &Path) -> String {
    let header = CONSOLE_LINK_PREFIX.trim_end();
    match read_range(link, root) {
        Some(scan) if scan.total == 0 => format!("{header} (none)"),
        Some(scan) => {
            let mut out = vec![header.to_owned()];
            for l in &scan.shown {
                out.push(format!("  {}", truncate(l, MAX_LINE_CHARS)));
            }
            if scan.total > scan.shown.len() {
                out.push(format!(
                    "  … {} more; call browser_console_messages for all",
                    scan.total - scan.shown.len()
                ));
            }
            out.join("\n")
        }
        None => format!("{header} (unavailable; call browser_console_messages)"),
    }
}

/// Parse `<path>#L<a>[-L<b>]` and scan those 1-based lines, if the path is
/// a file inside `root`.
fn read_range(link: &str, root: &Path) -> Option<Scan> {
    let (path, frag) = link.trim().rsplit_once("#L")?;
    let (from, to) = match frag.split_once("-L") {
        Some((a, b)) => (a.parse::<usize>().ok()?, b.parse::<usize>().ok()?),
        None => {
            let n = frag.parse::<usize>().ok()?;
            (n, n)
        }
    };
    if from == 0 || to < from {
        return None;
    }
    let file = std::fs::canonicalize(Path::new(path)).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    if !file.starts_with(&root) {
        return None;
    }
    let f = std::fs::File::open(&file).ok()?;
    scan_lines(f, from, to).ok()
}

/// What [`scan_lines`] kept from a line range.
#[derive(Debug)]
struct Scan {
    /// Lines in the requested range that exist in the file.
    total: usize,
    /// The first [`MAX_INLINE_LINES`] of them, each capped just past
    /// [`MAX_LINE_CHARS`] so [`truncate`] still marks the cut.
    shown: Vec<String>,
}

/// Stream lines `from..=to` (1-based) out of `src` with bounded memory: a
/// page can log one multi-megabyte line, or millions of lines, and neither
/// may be read whole into the proxy.
fn scan_lines(src: impl std::io::Read, from: usize, to: usize) -> std::io::Result<Scan> {
    use std::io::BufRead;
    // One char is at most 4 UTF-8 bytes; keep one char past the limit.
    let byte_cap = (MAX_LINE_CHARS + 1) * 4;
    let mut reader = std::io::BufReader::new(src);
    let mut scan = Scan {
        total: 0,
        shown: Vec::new(),
    };
    let mut line_no = 1usize;
    let mut buf: Vec<u8> = Vec::new();
    let mut in_line = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            if in_line {
                finish_line(&mut scan, &mut buf, line_no, from);
            }
            return Ok(scan);
        }
        let (part, ends_line) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (&chunk[..i], Some(i + 1)),
            None => (chunk, None),
        };
        let keep = line_no >= from && scan.shown.len() < MAX_INLINE_LINES;
        if keep && buf.len() < byte_cap {
            let room = byte_cap - buf.len();
            buf.extend_from_slice(&part[..part.len().min(room)]);
        }
        in_line = true;
        let used = ends_line.unwrap_or(chunk.len());
        reader.consume(used);
        if ends_line.is_some() {
            finish_line(&mut scan, &mut buf, line_no, from);
            in_line = false;
            if line_no >= to {
                return Ok(scan);
            }
            line_no += 1;
        }
    }
}

fn finish_line(scan: &mut Scan, buf: &mut Vec<u8>, line_no: usize, from: usize) {
    if line_no < from {
        return;
    }
    scan.total += 1;
    if scan.shown.len() < MAX_INLINE_LINES {
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        let text: String = String::from_utf8_lossy(buf)
            .chars()
            .take(MAX_LINE_CHARS + 1)
            .collect();
        scan.shown.push(text);
    }
    buf.clear();
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests;
