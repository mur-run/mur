//! Frozen contract for slice 3: the locator string grammar and snapshot resolution.
//!
//! A recorded `@ref` is diagnostic-only: it expires as soon as Playwright MCP
//! produces another accessibility snapshot.  Replay resolves a stable locator
//! against the current snapshot and returns its current ref.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locator {
    Role { role: String, name: Option<String> },
    TestId(String),
    Text(String),
    Label(String),
    Css(String),
}

impl Locator {
    /// Parse `kind:body`. Unknown kinds are an error; a bare string with no
    /// `kind:` prefix is treated as `css:` for compatibility with
    /// `browser_generate_locator` output.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let s = s.trim();
        let (kind, body) = match s.split_once(':') {
            Some((k, b)) if matches!(k, "role" | "testid" | "text" | "label" | "css") => (k, b),
            Some((k, _)) => anyhow::bail!("unknown locator kind {k:?} in {s:?}"),
            None => ("css", s),
        };
        anyhow::ensure!(!body.is_empty(), "empty locator body in {s:?}");
        Ok(match kind {
            "role" => {
                let (role, name) = match body.split_once('[') {
                    Some((r, rest)) => {
                        let rest = rest.trim_end_matches(']');
                        let name = rest
                            .strip_prefix("name=")
                            .map(|n| n.trim_matches('"').to_string());
                        (r.to_string(), name)
                    }
                    None => (body.to_string(), None),
                };
                Locator::Role { role, name }
            }
            "testid" => Locator::TestId(body.to_string()),
            "text" => Locator::Text(body.to_string()),
            "label" => Locator::Label(body.to_string()),
            _ => Locator::Css(body.to_string()),
        })
    }

    /// Canonical string form (what gets written to `actions.yaml`).
    pub fn to_string_canonical(&self) -> String {
        match self {
            Locator::Role {
                role,
                name: Some(n),
            } => format!("role:{role}[name=\"{n}\"]"),
            Locator::Role { role, name: None } => format!("role:{role}"),
            Locator::TestId(s) => format!("testid:{s}"),
            Locator::Text(s) => format!("text:{s}"),
            Locator::Label(s) => format!("label:{s}"),
            Locator::Css(s) => format!("css:{s}"),
        }
    }
}

/// An accessibility-snapshot node needed by deterministic replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotNode {
    pub reference: String,
    pub role: String,
    pub name: String,
    pub text: String,
    pub test_id: Option<String>,
    pub label: Option<String>,
}

/// Parse Playwright MCP's text accessibility snapshot into addressable nodes.
///
/// The MCP emits lines such as `- button "Sign in" [ref=e12]`; indentation is
/// intentionally ignored because locator resolution is about node attributes,
/// not tree ancestry.  `testid` is not normally exposed by the snapshot, but
/// is supported when the downstream supplies `[data-testid=foo]`.
pub fn parse_snapshot(snapshot: &str) -> Vec<SnapshotNode> {
    snapshot.lines().filter_map(parse_snapshot_line).collect()
}

fn parse_snapshot_line(line: &str) -> Option<SnapshotNode> {
    let reference = extract_bracket_value(line, "ref=")?;
    let before_ref = line.split("[ref=").next()?.trim();
    let before_ref = before_ref.trim_start_matches('-').trim();
    let mut words = before_ref.split_whitespace();
    let role = words.next()?.trim_matches(':').to_string();
    if role.is_empty() {
        return None;
    }

    let name = extract_quoted(before_ref).unwrap_or_default();
    let text = if name.is_empty() {
        words.collect::<Vec<_>>().join(" ")
    } else {
        name.clone()
    };
    Some(SnapshotNode {
        reference,
        role,
        name,
        text,
        test_id: extract_bracket_value(line, "data-testid="),
        label: extract_bracket_value(line, "label="),
    })
}

fn extract_bracket_value(line: &str, key: &str) -> Option<String> {
    let rest = line.split_once(key)?.1;
    let value = rest.split(']').next()?.trim();
    Some(value.trim_matches('"').to_string())
}

fn extract_quoted(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Resolve a stable locator to the current Playwright ref. CSS cannot be
/// resolved from an accessibility snapshot, so replay must hand it to
/// `browser_generate_locator` instead of guessing.
pub fn resolve(locator: &Locator, snapshot: &[SnapshotNode]) -> Option<String> {
    let node = snapshot.iter().find(|node| match locator {
        Locator::Role { role, name } => {
            node.role.eq_ignore_ascii_case(role)
                && name.as_ref().is_none_or(|name| node.name == *name)
        }
        Locator::TestId(id) => node.test_id.as_deref() == Some(id),
        Locator::Text(text) => node.text.contains(text) || node.name.contains(text),
        Locator::Label(label) => node.label.as_deref() == Some(label) || node.name == *label,
        Locator::Css(_) => false,
    })?;
    Some(node.reference.clone())
}

/// Produce stable replay candidates for a ref from the latest a11y snapshot.
/// Order is deliberate: semantic role first, then an explicit test id, label,
/// and finally visible text. A ref is never emitted because it is ephemeral.
pub fn candidates_for_ref(reference: &str, snapshot: &[SnapshotNode]) -> Vec<String> {
    let reference = reference.trim_start_matches('@');
    let Some(node) = snapshot
        .iter()
        .find(|node| node.reference.trim_start_matches('@') == reference)
    else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    if !node.role.is_empty() {
        let locator = if node.name.is_empty() {
            Locator::Role {
                role: node.role.clone(),
                name: None,
            }
        } else {
            Locator::Role {
                role: node.role.clone(),
                name: Some(node.name.clone()),
            }
        };
        candidates.push(locator.to_string_canonical());
    }
    if let Some(id) = &node.test_id {
        candidates.push(Locator::TestId(id.clone()).to_string_canonical());
    }
    if let Some(label) = &node.label {
        candidates.push(Locator::Label(label.clone()).to_string_canonical());
    }
    if !node.text.is_empty() {
        candidates.push(Locator::Text(node.text.clone()).to_string_canonical());
    }
    candidates.sort();
    candidates.dedup();
    // Restore the designed priority after deduplication rather than relying on
    // the lexical sort used above.
    candidates.sort_by_key(|candidate| match Locator::parse(candidate) {
        Ok(Locator::Role { .. }) => 0,
        Ok(Locator::TestId(_)) => 1,
        Ok(Locator::Label(_)) => 2,
        Ok(Locator::Text(_)) => 3,
        _ => 4,
    });
    candidates
}

/// SPEC §3.2 pruning rule: reject `nth-child` / `nth-of-type`, `>` chains
/// deeper than 3, and generated class names (`css-`, `sc-`, `_`, or
/// `[a-z]{1,2}\d{3,}`). Non-CSS kinds are always stable.
pub fn is_stable(s: &str) -> bool {
    let Ok(l) = Locator::parse(s) else {
        return false;
    };
    let Locator::Css(css) = l else {
        return true;
    };
    if css.contains(":nth-child") || css.contains(":nth-of-type") {
        return false;
    }
    if css.matches('>').count() > 3 {
        return false;
    }
    // every `.class` token
    for tok in css.split(|c: char| c.is_whitespace() || c == '>' || c == '[' || c == '#') {
        for cls in tok.split('.').skip(1) {
            let cls =
                cls.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
            if cls.starts_with("css-") || cls.starts_with("sc-") || cls.starts_with('_') {
                return false;
            }
            if looks_generated(cls) {
                return false;
            }
        }
    }
    true
}

/// `[a-z]{1,2}\d{3,}` e.g. `a1234`, `xy987`.
fn looks_generated(cls: &str) -> bool {
    let letters: String = cls.chars().take_while(|c| c.is_ascii_lowercase()).collect();
    if letters.is_empty() || letters.len() > 2 {
        return false;
    }
    let rest = &cls[letters.len()..];
    rest.len() >= 3 && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r#"
- banner:
  - link "MUR" [ref=e1]
  - button "Sign in" [ref=e2]
- main:
  - searchbox "Search docs" [ref=e3] [label="Search docs"]
  - button "Submit" [ref=e4] [data-testid=submit-button]
"#;

    #[test]
    fn parses_each_kind() {
        assert_eq!(
            Locator::parse("role:button[name=\"登入\"]").unwrap(),
            Locator::Role {
                role: "button".into(),
                name: Some("登入".into())
            }
        );
        assert_eq!(
            Locator::parse("role:searchbox").unwrap(),
            Locator::Role {
                role: "searchbox".into(),
                name: None
            }
        );
        assert_eq!(
            Locator::parse("testid:x").unwrap(),
            Locator::TestId("x".into())
        );
        assert_eq!(
            Locator::parse("text:搜尋").unwrap(),
            Locator::Text("搜尋".into())
        );
        assert_eq!(
            Locator::parse("label:密碼").unwrap(),
            Locator::Label("密碼".into())
        );
        assert_eq!(Locator::parse("css:#a").unwrap(), Locator::Css("#a".into()));
        assert_eq!(
            Locator::parse("#a > b").unwrap(),
            Locator::Css("#a > b".into())
        );
        assert!(Locator::parse("role:").is_err());
    }

    #[test]
    fn canonical_round_trip() {
        for s in [
            "role:button[name=\"登入\"]",
            "role:link",
            "testid:q",
            "text:t",
            "label:l",
            "css:#x",
        ] {
            assert_eq!(Locator::parse(s).unwrap().to_string_canonical(), s);
        }
    }

    #[test]
    fn stability_rules() {
        assert!(is_stable("role:button[name=\"x\"]"));
        assert!(is_stable("testid:anything.with-123"));
        assert!(is_stable("css:#login"));
        assert!(is_stable("css:form.login > button.primary"));
        assert!(!is_stable("css:ul > li:nth-child(3) > a"));
        assert!(!is_stable("css:div > div > div > div > a"));
        assert!(!is_stable("css:.css-1x2y3z"));
        assert!(!is_stable("css:.sc-bdVaJa"));
        assert!(!is_stable("css:._hidden"));
        assert!(!is_stable("css:.a1234"));
        assert!(
            is_stable("css:.abc1234"),
            "3 letters is not the generated pattern"
        );
        assert!(!is_stable("bogus:"));
    }

    #[test]
    fn resolves_role_testid_text_and_label_against_snapshot() {
        let nodes = parse_snapshot(SNAPSHOT);
        assert_eq!(
            resolve(
                &Locator::parse("role:button[name=\"Sign in\"]").unwrap(),
                &nodes
            ),
            Some("e2".into())
        );
        assert_eq!(
            resolve(&Locator::parse("testid:submit-button").unwrap(), &nodes),
            Some("e4".into())
        );
        assert_eq!(
            resolve(&Locator::parse("text:Search").unwrap(), &nodes),
            Some("e3".into())
        );
        assert_eq!(
            resolve(&Locator::parse("label:Search docs").unwrap(), &nodes),
            Some("e3".into())
        );
        assert_eq!(
            resolve(&Locator::parse("css:#submit").unwrap(), &nodes),
            None
        );
    }

    #[test]
    fn derives_ordered_candidates_from_current_ref() {
        let nodes = parse_snapshot(SNAPSHOT);
        assert_eq!(
            candidates_for_ref("@e4", &nodes),
            vec![
                "role:button[name=\"Submit\"]",
                "testid:submit-button",
                "text:Submit",
            ]
        );
        assert!(candidates_for_ref("@missing", &nodes).is_empty());
    }
}
