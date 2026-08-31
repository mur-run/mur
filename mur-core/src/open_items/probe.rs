//! Checking whether a reported item's own `next` command still finds anything.
//!
//! A reported item carries `next` — "the command or place that resolves it".
//! For most items that means *acting*: `git push`, `git commit`, `mur fleet
//! start`. Running those would perform the resolution rather than check it,
//! which is why nothing here executes an arbitrary `next`.
//!
//! Only the observational subset runs: `ls`, `test -f`, `git log`. They look
//! and report; they cannot change anything. That is the whole selection rule,
//! and it is worth stating because the allowlist reads arbitrary otherwise.
//!
//! **No shell.** The parser accepts one command with plain arguments and the
//! runner spawns it with an argv, so `&&`, `;`, `|`, backticks and `$()` are
//! not "rejected" — they are unrepresentable. A `next` containing any of them
//! simply does not parse, which is the same answer by a safer route.
//!
//! The result never resolves an item. It ranks: an item whose own check still
//! finds its artefact is more likely done, and a listing that says so is more
//! useful than one that does not — but "likely done" is not "done", and
//! closing something a person recorded on that evidence is the deletion this
//! module's neighbours already refuse to do.

use std::path::Path;

use mur_open_items::OpenItem;
use std::process::Command;

/// A `next` command that only looks.
#[derive(Debug, PartialEq)]
pub enum Probe {
    /// `ls <paths…>` — exits non-zero when a path is missing.
    Ls(Vec<String>),
    /// `test -f <path>` — the same question, spelled explicitly.
    TestF(String),
    /// `git log <args…>` — reads history, writes nothing.
    GitLog(Vec<String>),
}

/// Anything that would let a shell do more than run one command.
///
/// Checked against the raw string before splitting, so a metacharacter cannot
/// survive into an argument and be interpreted later by something that *does*
/// use a shell.
const SHELL_METACHARACTERS: [char; 10] = ['&', ';', '|', '<', '>', '`', '$', '(', ')', '\n'];

/// Parse `next` into something safe to run, or nothing.
///
/// Deliberately strict: the whole string must be one allowed command. Real
/// `next` values are frequently pipelines (`ls … | grep …; git status …`) or
/// actions (`cd … && git commit … && git push`), and those must not
/// half-parse into their first clause — running `cd` from `cd X && git push`
/// is harmless, but a parser that discards the tail invites one that does not.
pub fn parse_probe(next: &str) -> Option<Probe> {
    if next.chars().any(|c| SHELL_METACHARACTERS.contains(&c)) {
        return None;
    }
    let mut words = next.split_whitespace();
    match words.next()? {
        "ls" => {
            let args: Vec<String> = words
                .filter(|w| !w.starts_with('-'))
                .map(str::to_string)
                .collect();
            (!args.is_empty()).then_some(Probe::Ls(args))
        }
        "test" => match (words.next()?, words.next()) {
            ("-f", Some(path)) if words.next().is_none() => Some(Probe::TestF(path.to_string())),
            _ => None,
        },
        "git" => {
            (words.next()? == "log").then(|| Probe::GitLog(words.map(str::to_string).collect()))
        }
        _ => None,
    }
}

/// Run the probe. `Some(true)` = it still finds what it looks for.
///
/// `None` when the command could not be run at all, which is not evidence
/// either way and must not be reported as if it were.
pub fn run_probe(probe: &Probe, cwd: &Path) -> Option<bool> {
    let status = match probe {
        Probe::Ls(paths) => Command::new("ls").args(paths).current_dir(cwd).output(),
        Probe::TestF(path) => {
            // Answered directly rather than by spawning `test`: same question,
            // one syscall, and nothing to quote.
            return Some(cwd.join(path).is_file());
        }
        Probe::GitLog(args) => Command::new("git")
            .arg("log")
            .args(args)
            .current_dir(cwd)
            .output(),
    };
    let out = status.ok()?;
    match probe {
        // `git log` exits 0 on an empty range, so the exit code says nothing.
        // Output is the signal: commits were found, or none were.
        Probe::GitLog(_) => Some(!out.stdout.is_empty()),
        _ => Some(out.status.success()),
    }
}

/// Run each item's probe where it has one, and fold the answer into the title.
///
/// Ranking, not resolution: an item whose own check still finds its artefact
/// sorts last and says so, but stays in the list. Closing it would be deciding
/// on a proxy that a person's record is wrong — the deletion this module's
/// neighbours already refuse.
///
/// Items with no runnable probe are untouched and keep their position, so a
/// listing does not silently reorder around a check most items cannot answer.
pub fn annotate(items: Vec<OpenItem>, cwd: &Path) -> (Vec<OpenItem>, Coverage) {
    let mut cov = Coverage::default();
    let mut out: Vec<(bool, OpenItem)> = items
        .into_iter()
        .map(|mut it| {
            let found = match it.next.as_deref().and_then(parse_probe) {
                Some(p) => {
                    cov.checked += 1;
                    run_probe(&p, cwd)
                }
                None => {
                    cov.unchecked += 1;
                    None
                }
            };
            match found {
                Some(true) => {
                    it.title = format!("{} [its own check still finds this]", it.title);
                    (true, it)
                }
                _ => (false, it),
            }
        })
        .collect();
    // Stable, so everything else keeps the order `collect` gave it.
    out.sort_by_key(|(found, _)| *found);
    cov.satisfied = out.iter().filter(|(found, _)| *found).count();
    (out.into_iter().map(|(_, it)| it).collect(), cov)
}

/// What `--check` was able to answer.
///
/// Reported because a listing that looks unchanged after a check is
/// indistinguishable from one where nothing could be checked — and most `next`
/// values cannot be. Running a check and saying nothing about its reach is the
/// failure this whole module exists downstream of.
#[derive(Debug, Default, PartialEq)]
pub struct Coverage {
    /// Items whose `next` parsed into something runnable.
    pub checked: usize,
    /// Items with no `next`, or one that acts or needs a shell.
    pub unchecked: usize,
    /// Of those checked, how many still find what they look for.
    pub satisfied: usize,
}

impl Coverage {
    /// One line, or nothing when there was nothing to say.
    pub fn line(&self) -> Option<String> {
        (self.checked + self.unchecked > 0).then(|| {
            let mut s = format!(
                "checked {} of {} reported item{}",
                self.checked,
                self.checked + self.unchecked,
                if self.checked + self.unchecked == 1 {
                    ""
                } else {
                    "s"
                }
            );
            if self.unchecked > 0 {
                s.push_str(&format!(
                    " — {} have no runnable check (their `next` acts, or needs a shell)",
                    self.unchecked
                ));
            }
            if self.satisfied > 0 {
                s.push_str(&format!(
                    "; {} still find what they look for",
                    self.satisfied
                ));
            }
            s
        })
    }
}

#[cfg(test)]
mod tests {
    /// A listing that looks unchanged after `--check` is indistinguishable
    /// from one where nothing could be checked — and most `next` values cannot
    /// be. Saying nothing about the check's reach is the failure this module
    /// sits downstream of.
    #[test]
    fn coverage_says_how_much_of_the_list_it_could_answer_for() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("done.md"), "x").unwrap();
        let (_, cov) = annotate(
            vec![
                item("checkable and done", Some("test -f done.md")),
                item("checkable and not", Some("test -f missing.md")),
                item("acts", Some("git push")),
                item("no next", None),
            ],
            d.path(),
        );
        assert_eq!(
            cov,
            Coverage {
                checked: 2,
                unchecked: 2,
                satisfied: 1
            }
        );
        let line = cov.line().unwrap();
        assert!(line.contains("checked 2 of 4"), "{line}");
        assert!(line.contains("2 have no runnable check"), "{line}");
        assert!(line.contains("1 still find"), "{line}");
    }

    /// Nothing reported means nothing to say — a line reading "checked 0 of 0"
    /// after every run is how a status surface stops being read.
    #[test]
    fn an_empty_list_produces_no_coverage_line() {
        let d = tempfile::tempdir().unwrap();
        let (_, cov) = annotate(vec![], d.path());
        assert_eq!(cov.line(), None);
    }

    fn item(title: &str, next: Option<&str>) -> OpenItem {
        OpenItem {
            title: title.into(),
            next: next.map(str::to_string),
            source: mur_open_items::ItemSource::Reported,
            origin: "agent:mur".into(),
            at: chrono::Utc::now(),
        }
    }

    /// The line this feature must not cross. Evidence ranks; it never closes.
    /// Deciding on a proxy that a person's record is wrong is the deletion the
    /// ageing half already refuses to do.
    #[test]
    fn annotate_never_drops_an_item() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("done.md"), "x").unwrap();
        let before = vec![
            item("done thing", Some("test -f done.md")),
            item("undone thing", Some("test -f missing.md")),
            item("unprobeable", Some("git push")),
            item("no next at all", None),
        ];
        let (after, _) = annotate(before, d.path());
        assert_eq!(after.len(), 4, "{after:#?}");
    }

    /// An item whose own check still finds its artefact sorts last and says so
    /// — the point is that a reader can tell without running anything.
    #[test]
    fn a_satisfied_check_is_marked_and_sorted_last() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("done.md"), "x").unwrap();
        let (after, _) = annotate(
            vec![
                item("done thing", Some("test -f done.md")),
                item("undone thing", Some("test -f missing.md")),
            ],
            d.path(),
        );
        assert_eq!(after[0].title, "undone thing");
        assert!(after[1].title.starts_with("done thing"), "{:?}", after[1]);
        assert!(
            after[1].title.contains("still finds this"),
            "{:?}",
            after[1]
        );
    }

    /// Most items cannot be checked at all. Those must keep their place and
    /// their text, or the flag reorders a list around a question it did not
    /// ask of most of it.
    #[test]
    fn items_without_a_runnable_probe_are_untouched() {
        let d = tempfile::tempdir().unwrap();
        let (after, _) = annotate(
            vec![
                item("first", Some("cd x && git push")),
                item("second", None),
                item("third", Some("mur fleet start acme")),
            ],
            d.path(),
        );
        let titles: Vec<&str> = after.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["first", "second", "third"]);
    }

    use super::*;

    /// Every one of these is a real `next` from the author's own store. None
    /// may run — they pipe, they chain, or they act.
    #[test]
    fn the_next_values_that_actually_occur_do_not_parse() {
        for next in [
            "ls docs/superpowers/specs/ | grep unified-chat-redesign; git status --short docs/",
            "cd /Volumes/x/mur-model-gateway && git add -A && git commit && git push",
            "git add scripts/setup.sh Cargo.toml && git commit && git push",
            "ls -lt docs/superpowers/specs/ | head -3",
            "mur fleet start rust-solo",
        ] {
            assert_eq!(parse_probe(next), None, "must not run: {next}");
        }
    }

    #[test]
    fn the_observational_forms_parse() {
        assert_eq!(
            parse_probe("ls -l docs/spec.md"),
            Some(Probe::Ls(vec!["docs/spec.md".into()]))
        );
        assert_eq!(
            parse_probe("test -f docs/spec.md"),
            Some(Probe::TestF("docs/spec.md".into()))
        );
        assert_eq!(
            parse_probe("git log --oneline -1"),
            Some(Probe::GitLog(vec!["--oneline".into(), "-1".into()]))
        );
    }

    /// A shell metacharacter is not filtered out of the arguments — the whole
    /// string is refused. Salvaging the safe-looking prefix of a command that
    /// was written to do more is how a parser becomes an execution surface.
    #[test]
    fn a_metacharacter_refuses_the_whole_string() {
        for next in [
            "ls foo && rm -rf /",
            "ls $(whoami)",
            "ls `id`",
            "ls foo > /etc/passwd",
            "test -f a; curl evil.example",
        ] {
            assert_eq!(parse_probe(next), None, "{next}");
        }
    }

    /// `ls` with no path would list the working directory and always succeed,
    /// which is evidence about nothing.
    #[test]
    fn ls_without_a_path_is_not_a_probe() {
        assert_eq!(parse_probe("ls"), None);
        assert_eq!(parse_probe("ls -la"), None);
    }

    /// Only `git log`. `git push` shares the prefix and must not ride in on it.
    #[test]
    fn only_the_reading_git_subcommand_parses() {
        for next in ["git push", "git commit -m x", "git status", "git"] {
            assert_eq!(parse_probe(next), None, "{next}");
        }
    }

    #[test]
    fn test_f_answers_from_the_filesystem() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("there"), "x").unwrap();
        assert_eq!(
            run_probe(&Probe::TestF("there".into()), d.path()),
            Some(true)
        );
        assert_eq!(
            run_probe(&Probe::TestF("absent".into()), d.path()),
            Some(false)
        );
    }

    #[test]
    fn ls_reports_a_missing_path_as_not_found() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("there"), "x").unwrap();
        assert_eq!(
            run_probe(&Probe::Ls(vec!["there".into()]), d.path()),
            Some(true)
        );
        assert_eq!(
            run_probe(&Probe::Ls(vec!["absent".into()]), d.path()),
            Some(false)
        );
    }
}
