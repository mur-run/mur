use super::*;

/// MUR's signature on outgoing work, stored under `attribution:` in
/// `~/.mur/config.yaml`.
///
/// Both surfaces are agent *behavior*, not runtime behavior: no Rust code in
/// this repo authors a user's commit or opens their PR — an agent does it by
/// shelling out to `git` and `gh`. So the only lever is the system prompt, and
/// this config is its single source of truth. Putting the text in a skill body
/// instead would fork it: skill markdown is loaded verbatim, with no template
/// expansion, so a signature written there could never follow this file.
///
/// The two knobs default asymmetrically on purpose. A PR body is a message
/// MUR is writing, and a credit line in it is ours to place. A commit trailer
/// is written permanently into a repository's history — often someone else's —
/// where it outlives the PR, shows up in `git log` and `git blame` forever, and
/// cannot be edited away without a rewrite. Opt-in is the honest default for
/// the second one.
///
/// There is no `enabled` flag, because with per-surface strings it would be a
/// second switch that can disagree with the first ("enabled: true" plus an
/// empty `pr` — which wins?). An empty or whitespace-only string IS the off
/// switch, and it is off for exactly one surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AttributionConfig {
    /// Appended to PR bodies the agent creates. Empty = no PR signature.
    pub pr: Option<String>,
    /// Appended as a trailer to commits the agent makes. `None`/empty = off,
    /// which is the default; see the type docs for why this one is opt-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// The shipped PR credit line.
pub const DEFAULT_PR_ATTRIBUTION: &str = "Generated with [MUR](https://app.mur.run/products/mur)";

impl Default for AttributionConfig {
    fn default() -> Self {
        Self {
            pr: Some(DEFAULT_PR_ATTRIBUTION.to_string()),
            commit: None,
        }
    }
}

/// `Some(trimmed)` only when the value carries actual text — the shared
/// "empty means off" rule for both surfaces.
fn signature(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

impl AttributionConfig {
    /// The PR credit line, or `None` when the user switched it off.
    pub fn pr_signature(&self) -> Option<&str> {
        signature(&self.pr)
    }

    /// The commit trailer, or `None` (the default) when off.
    pub fn commit_signature(&self) -> Option<&str> {
        signature(&self.commit)
    }

    /// The system-prompt rule, or `None` when both surfaces are off so a user
    /// who wants no signature pays no tokens for one.
    ///
    /// The signature text is quoted verbatim rather than described, so the
    /// model copies it instead of paraphrasing a credit line into something
    /// that is not the configured one.
    pub fn prompt_fragment(&self) -> Option<String> {
        let (pr, commit) = (self.pr_signature(), self.commit_signature());
        if pr.is_none() && commit.is_none() {
            return None;
        }
        let mut s = String::from(
            "\n\n## MUR attribution\n\
             When you publish work on the user's behalf, sign it with the exact text below — \
             copy it verbatim, do not reword it, and add it once.\n",
        );
        if let Some(pr) = pr {
            s.push_str(&format!(
                "- Opening a pull request (`gh pr create`, or any PR/MR body you author): \
                 end the body with its own line reading:\n  {pr}\n"
            ));
        }
        if let Some(commit) = commit {
            s.push_str(&format!(
                "- Making a commit (`git commit`): add this as a trailer in the last paragraph \
                 of the message, after a blank line:\n  {commit}\n"
            ));
        }
        s.push_str(
            "Never add a signature the user did not configure, and never sign a surface that is \
             not listed here.",
        );
        Some(s)
    }
}

#[cfg(test)]
mod attribution_tests {
    use crate::config::{AttributionConfig, Config};

    /// The shipped default: a PR gets a MUR credit line, a commit does not.
    /// The asymmetry is the whole design — a PR body is ours to sign, a commit
    /// trailer is written permanently into someone else's git history.
    #[test]
    fn pr_signature_ships_on_and_commit_trailer_ships_off() {
        let c: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(
            c.attribution.pr.as_deref(),
            Some("Generated with [MUR](https://app.mur.run/products/mur)")
        );
        assert_eq!(c.attribution.commit, None);
    }

    /// Both surfaces are independently overridable, and an empty string is the
    /// off switch — there is no separate `enabled` flag to disagree with.
    #[test]
    fn empty_string_is_the_off_switch_per_surface() {
        let c: Config = serde_yaml_ng::from_str("attribution:\n  pr: \"\"\n").unwrap();
        assert!(c.attribution.pr_signature().is_none());

        let c: Config = serde_yaml_ng::from_str(
            "attribution:\n  pr: \"   \"\n  commit: \"Co-Authored-By: MUR <noreply@mur.run>\"\n",
        )
        .unwrap();
        assert!(
            c.attribution.pr_signature().is_none(),
            "whitespace-only counts as off, not as a signature of spaces"
        );
        assert_eq!(
            c.attribution.commit_signature(),
            Some("Co-Authored-By: MUR <noreply@mur.run>")
        );
    }

    #[test]
    fn a_custom_pr_signature_replaces_the_default() {
        let c: Config = serde_yaml_ng::from_str("attribution:\n  pr: \"Made by ACME\"\n").unwrap();
        assert_eq!(c.attribution.pr_signature(), Some("Made by ACME"));
    }

    /// New config files advertise the default PR signature, but do not write a
    /// misleading `commit: null`: absence is the explicit opt-in default.
    #[test]
    fn serialization_ships_pr_attribution_but_omits_disabled_commit_trailer() {
        let yaml = serde_yaml::to_string(&Config::default()).unwrap();
        assert!(
            yaml.contains(
                "attribution:\n  pr: Generated with [MUR](https://app.mur.run/products/mur)"
            ),
            "{yaml}"
        );
        assert!(!yaml.contains("commit:"), "{yaml}");
    }

    /// Nothing to say → nothing injected. A user who blanks both surfaces must
    /// not pay system-prompt tokens for an empty rule.
    #[test]
    fn prompt_fragment_is_none_when_both_surfaces_are_off() {
        let off = AttributionConfig {
            pr: Some(String::new()),
            commit: None,
        };
        assert_eq!(off.prompt_fragment(), None);
    }

    /// The fragment names the surface it governs and quotes the text verbatim,
    /// so the model has no room to paraphrase the credit line.
    #[test]
    fn prompt_fragment_quotes_each_enabled_surface_verbatim() {
        let c = AttributionConfig::default();
        let f = c.prompt_fragment().expect("pr is on by default");
        assert!(f.contains("## MUR attribution"), "{f}");
        assert!(
            f.contains("Generated with [MUR](https://app.mur.run/products/mur)"),
            "the exact signature must appear, not a description of it: {f}"
        );
        assert!(f.contains("gh pr create"), "{f}");
        assert!(
            !f.to_lowercase().contains("git commit"),
            "commit trailer is off by default, so its instruction must be absent: {f}"
        );

        let both = AttributionConfig {
            pr: Some("Generated with MUR".into()),
            commit: Some("Co-Authored-By: MUR <noreply@mur.run>".into()),
        };
        let f = both.prompt_fragment().unwrap();
        assert!(f.contains("git commit"), "{f}");
        assert!(f.contains("Co-Authored-By: MUR <noreply@mur.run>"), "{f}");
    }

    /// Commit-only is a real configuration: a user may keep their PR bodies
    /// clean and still credit MUR in the trailer.
    #[test]
    fn commit_only_configuration_injects_only_the_commit_rule() {
        let c = AttributionConfig {
            pr: Some(String::new()),
            commit: Some("Co-Authored-By: MUR <noreply@mur.run>".into()),
        };
        let f = c.prompt_fragment().unwrap();
        assert!(f.contains("git commit"), "{f}");
        assert!(!f.contains("gh pr create"), "{f}");
    }
}
