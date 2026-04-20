//! Pattern-name macro reference detection (spec §4.4).
//!
//! Enumerates ~/.mur/patterns/*.yaml names, scans extractive spans and the
//! abstractive narrative for word-boundary matches, rewrites them to
//! {{pattern: <name>}}, and records (version, sha) per referenced pattern.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

use super::extractive::ExtractiveSpan;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MacroRef {
    pub name: String,
    pub pattern_version: u32,
    pub pattern_sha: String,
    pub marker: String,
}

pub fn detect_and_rewrite(
    extractive: &mut [ExtractiveSpan],
    abstractive: &mut String,
    patterns_dir: &Path,
) -> Result<Vec<MacroRef>> {
    let names = enumerate_pattern_names(patterns_dir)?;
    if names.is_empty() {
        return Ok(Vec::new());
    }

    // Case-insensitive Aho-Corasick. Word-boundary enforced via post-check.
    let ac = aho_corasick::AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build(&names)
        .context("build aho-corasick")?;

    let mut found: BTreeSet<String> = BTreeSet::new();

    for span in extractive.iter_mut() {
        let new_text = rewrite_with_markers(&span.text, &ac, &names, &mut found);
        span.text = new_text;
    }
    let new_narrative = rewrite_with_markers(abstractive, &ac, &names, &mut found);
    *abstractive = new_narrative;

    let mut refs = Vec::new();
    for name in found {
        let (version, sha) = read_pattern_meta(patterns_dir, &name).unwrap_or_else(|e| {
            tracing::warn!("failed to read pattern {name}: {e:#}; using defaults");
            (0, String::new())
        });
        refs.push(MacroRef {
            name: name.clone(),
            pattern_version: version,
            pattern_sha: sha,
            marker: format!("{{{{pattern: {name}}}}}"),
        });
    }
    Ok(refs)
}

fn enumerate_pattern_names(patterns_dir: &Path) -> Result<Vec<String>> {
    if !patterns_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(patterns_dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            out.push(stem.to_string());
        }
    }
    Ok(out)
}

fn rewrite_with_markers(
    text: &str,
    ac: &aho_corasick::AhoCorasick,
    names: &[String],
    found: &mut BTreeSet<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let bytes = text.as_bytes();

    for mat in ac.find_iter(text) {
        let start = mat.start();
        let end = mat.end();
        let name = &names[mat.pattern()];

        // Word-boundary check. Chars before/after must not be ASCII alphanumeric
        // or '-' (pattern names use kebab-case).
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);

        // Code-fence / backtick / YAML-quote skip.
        if !before_ok || !after_ok || inside_code_or_quote(text, start) {
            continue;
        }

        out.push_str(&text[last..start]);
        out.push_str(&format!("{{{{pattern: {name}}}}}"));
        last = end;
        found.insert(name.clone());
    }
    out.push_str(&text[last..]);
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn inside_code_or_quote(text: &str, pos: usize) -> bool {
    // Toggle state up to `pos` for each of: backtick, code-fence (```), single-quote, double-quote
    let mut in_backtick = false;
    let mut in_code_fence = false;
    let mut in_single = false;
    let mut in_double = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < pos {
        if i + 3 <= pos && &bytes[i..i + 3] == b"```" {
            in_code_fence = !in_code_fence;
            i += 3;
            continue;
        }
        match bytes[i] {
            b'`' if !in_code_fence => in_backtick = !in_backtick,
            b'\'' if !in_code_fence && !in_backtick => in_single = !in_single,
            b'"' if !in_code_fence && !in_backtick => in_double = !in_double,
            _ => {}
        }
        i += 1;
    }
    in_backtick || in_code_fence || in_single || in_double
}

fn read_pattern_meta(patterns_dir: &Path, name: &str) -> Result<(u32, String)> {
    let path = patterns_dir.join(format!("{name}.yaml"));
    let bytes = std::fs::read(&path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha = hex::encode(hasher.finalize());
    // schema version lives at top level; if missing, default 0
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&bytes).unwrap_or(serde_yaml::Value::Null);
    let version = yaml.get("schema").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Ok((version, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Role, Source};

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_pattern(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(format!("{name}.yaml")), body).unwrap();
    }

    fn span(text: &str) -> ExtractiveSpan {
        ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: 1,
            text: text.into(),
            src: Source::ClaudeCode,
        }
    }

    #[test]
    fn detects_and_rewrites_known_pattern() {
        let tmp = tmpdir();
        write_pattern(
            tmp.path(),
            "atomic-yaml-write",
            "schema: 2\nname: atomic-yaml-write\n",
        );
        let mut spans = vec![span("we used atomic-yaml-write for the writer.")];
        let mut narr = "The choice was atomic-yaml-write.".to_string();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "atomic-yaml-write");
        assert_eq!(refs[0].pattern_version, 2);
        assert!(spans[0].text.contains("{{pattern: atomic-yaml-write}}"));
        assert!(narr.contains("{{pattern: atomic-yaml-write}}"));
    }

    #[test]
    fn word_boundary_skips_partial_match() {
        let tmp = tmpdir();
        write_pattern(tmp.path(), "rust", "schema: 1\n");
        let mut spans = vec![span("my rustic approach is rustless actually")];
        let mut narr = String::new();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert!(
            refs.is_empty(),
            "rust should not match 'rustic' or 'rustless'"
        );
        assert!(!spans[0].text.contains("{{pattern"));
    }

    #[test]
    fn skips_inside_backticks() {
        let tmp = tmpdir();
        write_pattern(tmp.path(), "my-pattern", "schema: 1\n");
        let mut spans = vec![span("reference to `my-pattern` is literal")];
        let mut narr = String::new();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn empty_patterns_dir_returns_empty() {
        let tmp = tmpdir();
        let mut spans = vec![span("anything")];
        let mut narr = "anything".to_string();
        let refs = detect_and_rewrite(&mut spans, &mut narr, tmp.path()).unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn nonexistent_patterns_dir_is_safe() {
        let mut spans = vec![span("x")];
        let mut narr = "x".to_string();
        let refs =
            detect_and_rewrite(&mut spans, &mut narr, Path::new("/nonexistent/path")).unwrap();
        assert!(refs.is_empty());
    }
}
