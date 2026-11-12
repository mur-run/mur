//! Resolve a project's current git HEAD commit, in-process (no subprocess).

use std::path::Path;

/// Commit id HEAD points at for the repository rooted at exactly `path`.
///
/// `Repository::open` is used on purpose — no discovery upward — so a project
/// nested inside another repo never reports its parent's HEAD. Linked
/// worktrees (`.git` file), packed refs and detached HEAD all resolve; a
/// non-repo, an unborn branch or any error yields `None`.
pub fn resolve(path: &Path) -> Option<String> {
    let repo = git2::Repository::open(path).ok()?;
    let head = repo.head().ok()?;
    Some(head.peel_to_commit().ok()?.id().to_string())
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use git2::{Oid, Repository, Signature};
    use std::path::Path;

    fn commit(repo: &Repository, msg: &str) -> Oid {
        let sig = Signature::now("t", "t@example.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<_> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap()
    }

    fn init(dir: &Path) -> (Repository, Oid) {
        let repo = Repository::init(dir).unwrap();
        let oid = commit(&repo, "first");
        (repo, oid)
    }

    #[test]
    fn primary_worktree_resolves_head() {
        let t = tempfile::tempdir().unwrap();
        let (_repo, oid) = init(t.path());
        assert_eq!(resolve(t.path()), Some(oid.to_string()));
    }

    #[test]
    fn linked_worktree_resolves_its_own_head() {
        let t = tempfile::tempdir().unwrap();
        let main = t.path().join("main");
        let (repo, first) = init(&main);
        let wt_path = t.path().join("wt");
        let wt = repo.worktree("wt", &wt_path, None).unwrap();
        let wt_repo = Repository::open_from_worktree(&wt).unwrap();
        let second = commit(&wt_repo, "on the worktree branch");
        assert_ne!(first, second);
        assert_eq!(resolve(&wt_path), Some(second.to_string()));
        assert_eq!(resolve(&main), Some(first.to_string()));
    }

    #[test]
    fn packed_ref_resolves() {
        let t = tempfile::tempdir().unwrap();
        let (repo, oid) = init(t.path());
        let branch = repo.head().unwrap().name().unwrap().to_string();
        let git_dir = repo.path().to_path_buf();
        drop(repo);
        // Pack the branch ref by hand and delete the loose ref, as `git gc` does.
        std::fs::write(
            git_dir.join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled sorted \n{oid} {branch}\n"),
        )
        .unwrap();
        std::fs::remove_file(git_dir.join(&branch)).unwrap();
        assert!(!git_dir.join(&branch).exists());
        assert_eq!(resolve(t.path()), Some(oid.to_string()));
    }

    #[test]
    fn detached_head_resolves() {
        let t = tempfile::tempdir().unwrap();
        let (repo, first) = init(t.path());
        let _second = commit(&repo, "second");
        repo.set_head_detached(first).unwrap();
        assert!(repo.head_detached().unwrap());
        assert_eq!(resolve(t.path()), Some(first.to_string()));
    }

    #[test]
    fn non_repo_unborn_and_nested_dir_are_none() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(resolve(t.path()), None, "plain dir");

        let unborn = t.path().join("unborn");
        Repository::init(&unborn).unwrap();
        assert_eq!(resolve(&unborn), None, "unborn branch");

        let outer = t.path().join("outer");
        init(&outer);
        let nested = outer.join("sub");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(resolve(&nested), None, "no discovery upward");
    }
}
