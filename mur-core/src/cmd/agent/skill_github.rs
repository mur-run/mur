//! Install a multi-file skill from a GitHub directory URL: clone, convert each
//! SKILL.md to a MUR manifest, scan bundled scripts (flag-not-block), install.
//! Scripts are copied, never executed.

use anyhow::{Result, anyhow, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// A GitHub repo or directory URL resolved to clone inputs. An empty `git_ref`
/// means the repository default branch.
#[derive(Debug, Clone, PartialEq)]
pub struct GithubDir {
    pub clone_url: String,
    pub git_ref: String,
    pub subdir: String,
}

/// Recognize a github.com repo or directory URL. Returns `None` for any other
/// host or path shape so the caller falls through to the single-file path.
///
/// Accepted:
/// - `github.com/<owner>/<repo>`               → default branch, repo root
/// - `github.com/<owner>/<repo>/tree/<ref>/<path...>` → that ref + subdir
///
/// ponytail: a `tree/<ref>` whose branch name itself contains `/`
/// (e.g. `feature/x`) is mis-split; single-segment refs (the common case) work.
pub fn parse_github_dir(url: &str) -> Option<GithubDir> {
    let u = reqwest::Url::parse(url.trim()).ok()?;
    if u.host_str()? != "github.com" {
        return None;
    }
    let segs: Vec<&str> = u
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() < 2 {
        return None;
    }
    let owner = segs[0];
    let repo = segs[1].trim_end_matches(".git");
    let clone_url = format!("https://github.com/{owner}/{repo}.git");
    if segs.len() == 2 {
        return Some(GithubDir { clone_url, git_ref: String::new(), subdir: String::new() });
    }
    if segs.len() >= 4 && segs[2] == "tree" {
        return Some(GithubDir {
            clone_url,
            git_ref: segs[3].to_string(),
            subdir: segs[4..].join("/"),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_dir_forms() {
        let tree = parse_github_dir(
            "https://github.com/obra/superpowers/tree/main/skills/brainstorming",
        )
        .unwrap();
        assert_eq!(tree.clone_url, "https://github.com/obra/superpowers.git");
        assert_eq!(tree.git_ref, "main");
        assert_eq!(tree.subdir, "skills/brainstorming");

        let bare = parse_github_dir("https://github.com/obra/superpowers").unwrap();
        assert_eq!(bare.git_ref, "");
        assert_eq!(bare.subdir, "");

        assert!(parse_github_dir("https://example.com/a/b/tree/main/x").is_none());
        assert!(parse_github_dir("https://github.com/only-owner").is_none());
        assert!(parse_github_dir("https://github.com/o/r/blob/main/skill.yaml").is_none());
    }
}
