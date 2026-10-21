//! Heredoc bodies that are data, not commands, removed before tier scanning.
//!
//! `tool_tier` matches deny-listed words anywhere in a bash command. A file
//! written with `cat > f <<'EOF' … EOF` whose source merely *mentions*
//! `launchctl` or `chmod` was therefore tiered Privileged (seen live while an
//! agent wrote a launchd fix, #2015). The body of a heredoc fed to a plain
//! data sink is never executed, so it is dropped from the scanned text.
//!
//! Fail-closed by construction (#2028 review): this can only ever LOWER a
//! tier, so every doubt returns the command untouched. Bodies are dropped
//! only when ALL of:
//! - the `<<` is a real operator — not quoted, not in a `#` comment, not in
//!   `${…}`/`$((…))` (any `$(`, `${`, `$[`, `$'`, backtick, paren or brace in
//!   the command text abandons the scan outright);
//! - the command owning it is `cat`/`tee` by name or from `/bin`, `/usr/bin`;
//! - every command in the whole text is an inert external head (`cd`,
//!   `mkdir`, `cat`, `tee`, …). That rules out a pipe onward, running what
//!   was just written (`bash a.sh`, `make`, git hooks), and anything that
//!   could redefine `cat` (`alias`, `hash`, `export`, `PATH=…`, functions);
//! - an unquoted delimiter's body has no `$(`, `${`, `$[`, backtick or
//!   backslash-newline (bash expands those before `cat` sees them);
//! - its terminator line is present, matched exactly as bash matches it.
//!
//! The original command still goes to `opaque_shell` untouched.

use std::borrow::Cow;

/// Commands that only copy their stdin somewhere; a heredoc fed to one is data.
const DATA_SINKS: &[&str] = &["cat", "tee"];

/// The only heads that may share a command with a dropped body: external
/// commands (or `cd`/`echo`/`true`) that can neither execute a file just
/// written nor change what `cat` resolves to.
const INERT_HEADS: &[&str] = &["cat", "tee", "cd", "mkdir", "echo", "true", "pwd", "ls"];

/// Directories a path-qualified head may come from (`/bin/cat`, not `./cat`).
const SYSTEM_BINS: &[&str] = &["/bin/", "/usr/bin/"];

/// What an unquoted heredoc body may not contain: bash expands these (or, for
/// backslash-newline, joins lines and so moves the terminator).
const EXPANSIONS: &[&str] = &["`", "$(", "${", "$[", "\\\n"];

struct Pending {
    delim: String,
    dash: bool,
    quoted: bool,
    sink: bool,
}

/// `cmd` with the bodies of data-sink heredocs removed (see module docs).
pub(super) fn scan_text(cmd: &str) -> Cow<'_, str> {
    if !cmd.contains("<<") {
        return Cow::Borrowed(cmd);
    }
    Scanner::default()
        .run(cmd)
        .map_or(Cow::Borrowed(cmd), Cow::Owned)
}

/// One pass over the command text. `None` from any step means "not proved":
/// the caller then scans the whole command, as before this module existed.
#[derive(Default)]
struct Scanner {
    out: String,
    /// The current word with quotes removed.
    word: String,
    in_word: bool,
    /// The word being built is a redirection target, not a command head.
    redir_target: bool,
    head: Option<String>,
    heads: Vec<String>,
    pending: Vec<Pending>,
}

impl Scanner {
    fn run(mut self, cmd: &str) -> Option<String> {
        let mut rest = cmd;
        while let Some(c) = next(&mut rest) {
            match c {
                '\'' => {
                    let end = rest.find('\'')?;
                    self.out.push(c);
                    self.out.push_str(&rest[..=end]);
                    self.word.push_str(&rest[..end]);
                    self.in_word = true;
                    rest = &rest[end + 1..];
                }
                '"' => {
                    self.out.push(c);
                    self.in_word = true;
                    self.double_quoted(&mut rest)?;
                }
                '\\' => {
                    self.out.push(c);
                    if let Some(n) = next(&mut rest) {
                        self.out.push(n);
                        if n != '\n' {
                            self.word.push(n);
                            self.in_word = true;
                        }
                    }
                }
                '$' if matches!(peek(rest), Some('(' | '{' | '[' | '\'' | '"')) => return None,
                '`' | '(' | ')' | '{' | '}' => return None,
                '|' if peek(rest) != Some('|') => return None,
                '#' if !self.in_word => {
                    let end = rest.find('\n').unwrap_or(rest.len());
                    self.out.push(c);
                    self.out.push_str(&rest[..end]);
                    rest = &rest[end..];
                }
                ' ' | '\t' => {
                    self.out.push(c);
                    self.end_word();
                }
                '&' if peek(rest) == Some('>') => {
                    self.out.push(c);
                    self.redirect(&mut rest);
                }
                '|' | '&' | ';' => {
                    self.out.push(c);
                    if peek(rest) == Some(c) {
                        self.out.push(c);
                        next(&mut rest);
                    }
                    self.end_segment();
                }
                '>' => {
                    self.out.push(c);
                    self.end_word();
                    self.redirect(&mut rest);
                }
                '<' => {
                    self.out.push(c);
                    self.end_word();
                    if peek(rest) != Some('<') {
                        self.redirect(&mut rest);
                    } else {
                        self.out.push(next(&mut rest)?);
                        if peek(rest) == Some('<') {
                            // `<<<` here-string: its word stays on this line.
                            self.out.push(next(&mut rest)?);
                            self.redir_target = true;
                        } else {
                            self.heredoc(&mut rest)?;
                        }
                    }
                }
                '\n' => {
                    self.out.push(c);
                    self.end_segment();
                    if !self.pending.is_empty() {
                        rest = self.take_bodies(rest)?;
                    }
                }
                _ => {
                    self.out.push(c);
                    self.word.push(c);
                    self.in_word = true;
                }
            }
        }
        self.end_segment();
        let proved = self.pending.is_empty() && self.heads.iter().all(|h| is_head(h, INERT_HEADS));
        proved.then_some(self.out)
    }

    /// Inside `"…"`: copied through; an expansion that runs code abandons.
    fn double_quoted(&mut self, rest: &mut &str) -> Option<()> {
        loop {
            let c = next(rest)?;
            self.out.push(c);
            match c {
                '"' => return Some(()),
                '`' => return None,
                '$' if matches!(peek(rest), Some('(' | '{' | '[')) => return None,
                '\\' => {
                    let n = next(rest)?;
                    self.out.push(n);
                    self.word.push(n);
                }
                _ => self.word.push(c),
            }
        }
    }

    /// The rest of a `>`, `>>`, `>&`, `>|`, `<&`, `<>`, `&>` operator.
    fn redirect(&mut self, rest: &mut &str) {
        while let Some(n) = peek(rest).filter(|n| matches!(n, '>' | '&' | '|')) {
            self.out.push(n);
            next(rest);
        }
        self.redir_target = true;
    }

    /// After `<<`: register the heredoc whose body starts on the next line.
    fn heredoc(&mut self, rest: &mut &str) -> Option<()> {
        let dash = peek(rest) == Some('-');
        if dash {
            self.out.push(next(rest)?);
        }
        while let Some(b) = peek(rest).filter(|b| matches!(b, ' ' | '\t')) {
            self.out.push(b);
            next(rest);
        }
        let (delim, quoted, len) = delimiter(rest)?;
        self.out.push_str(&rest[..len]);
        *rest = &rest[len..];
        let sink = self.head.as_deref().is_some_and(|h| is_head(h, DATA_SINKS));
        self.pending.push(Pending {
            delim,
            dash,
            quoted,
            sink,
        });
        Some(())
    }

    /// Consume each pending body through its terminator line. A sink body is
    /// dropped; any other body is copied through so the tier scan sees it.
    fn take_bodies<'a>(&mut self, mut rest: &'a str) -> Option<&'a str> {
        for p in std::mem::take(&mut self.pending) {
            let (body_end, term_end) = terminator(rest, &p)?;
            if !p.quoted && EXPANSIONS.iter().any(|e| rest[..body_end].contains(e)) {
                return None;
            }
            let from = if p.sink { body_end } else { 0 };
            self.out.push_str(&rest[from..term_end]);
            rest = &rest[term_end..];
        }
        Some(rest)
    }

    fn end_word(&mut self) {
        if !self.in_word {
            return;
        }
        self.in_word = false;
        let w = std::mem::take(&mut self.word);
        if !std::mem::take(&mut self.redir_target) && self.head.is_none() {
            self.head = Some(w);
        }
    }

    fn end_segment(&mut self) {
        self.end_word();
        self.redir_target = false;
        self.heads.extend(self.head.take());
    }
}

fn next(rest: &mut &str) -> Option<char> {
    let c = rest.chars().next()?;
    *rest = &rest[c.len_utf8()..];
    Some(c)
}

fn peek(rest: &str) -> Option<char> {
    rest.chars().next()
}

/// Is `head` one of `names`, bare or from a system bin directory?
fn is_head(head: &str, names: &[&str]) -> bool {
    let name = if head.contains('/') {
        SYSTEM_BINS.iter().find_map(|d| head.strip_prefix(d))
    } else {
        Some(head)
    };
    name.is_some_and(|n| names.contains(&n))
}

/// The delimiter word at the start of `rest`, quote-removed the way bash does
/// it: `(word, was any part quoted, bytes spanned)`. Only blanks, newline and
/// operators end it — a carriage return is part of the word, as in bash.
fn delimiter(rest: &str) -> Option<(String, bool, usize)> {
    let mut delim = String::new();
    let mut quoted = false;
    let mut chars = rest.char_indices();
    let mut end = rest.len();
    while let Some((i, c)) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')' => {
                end = i;
                break;
            }
            '$' | '`' => return None,
            '\'' => {
                quoted = true;
                loop {
                    match chars.next()?.1 {
                        '\'' => break,
                        q => delim.push(q),
                    }
                }
            }
            '"' => {
                quoted = true;
                loop {
                    match chars.next()?.1 {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()?.1 {
                            '\n' => return None,
                            n @ ('"' | '\\') => delim.push(n),
                            n => {
                                delim.push('\\');
                                delim.push(n);
                            }
                        },
                        q => delim.push(q),
                    }
                }
            }
            '\\' => {
                quoted = true;
                match chars.next()?.1 {
                    '\n' => return None,
                    n => delim.push(n),
                }
            }
            _ => delim.push(c),
        }
    }
    (!delim.is_empty()).then_some((delim, quoted, end))
}

/// `(body end, terminator line end)` in `rest`: the first line that IS the
/// delimiter — exactly, or after leading tabs for `<<-`. Never trimmed
/// otherwise: bash treats `  EOF` as body.
fn terminator(rest: &str, p: &Pending) -> Option<(usize, usize)> {
    let mut pos = 0;
    while pos < rest.len() {
        let line_end = rest[pos..].find('\n').map_or(rest.len(), |i| pos + i);
        let line = &rest[pos..line_end];
        let line = if p.dash {
            line.trim_start_matches('\t')
        } else {
            line
        };
        let next = (line_end + 1).min(rest.len());
        if line == p.delim {
            return Some((pos, next));
        }
        pos = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::scan_text;

    #[test]
    fn a_sink_body_is_dropped_but_the_command_line_stays() {
        let s = scan_text("cd /r && cat > f.rs <<'EOF'\nlaunchctl x\nEOF\nls -la");
        assert!(!s.contains("launchctl"), "{s}");
        assert!(s.contains("cat > f.rs") && s.contains("ls -la"), "{s}");
    }

    #[test]
    fn bodies_that_may_execute_are_kept() {
        for cmd in [
            "bash <<'EOF'\nlaunchctl x\nEOF",
            "cat <<EOF | sh\nlaunchctl x\nEOF",
            "python3 - <<EOF\nlaunchctl x\nEOF",
            // No terminator: cannot tell where the body ends.
            "cat > f <<EOF\nlaunchctl x",
            // `<<` inside quotes is literal; the next line is a real command.
            "echo \"; cat <<X\"\nlaunchctl x\nX",
        ] {
            assert!(scan_text(cmd).contains("launchctl"), "cmd: {cmd}");
        }
    }

    #[test]
    fn here_strings_and_tee_are_handled() {
        assert_eq!(scan_text("cat <<< \"$X\""), "cat <<< \"$X\"");
        let s = scan_text("tee a.txt <<-EOF >/dev/null\n\tchmod 777 x\n\tEOF");
        assert!(!s.contains("chmod"), "{s}");
    }

    /// End to end through the classifier: source that merely mentions a
    /// privileged word, written to a file, must not tier the write as
    /// Privileged. Seen live: an agent writing a launchd fix (#2015).
    #[test]
    fn a_heredoc_written_to_a_file_is_not_a_command() {
        let cmd = "cd /repo && cat > src/refusal.rs <<'EOF'\n\
                   //! The plist is KeepAlive, so launchctl respawns it.\n\
                   //! Do not chmod the socket.\n\
                   EOF";
        let t =
            crate::hitl::tool_tier::classify("bash", Some(&serde_json::json!({ "command": cmd })));
        assert!(crate::hitl::tier_may_be_granted(t), "escalated to {t:?}");
    }

    fn tier_of(cmd: &str) -> crate::hitl::RiskTier {
        crate::hitl::tool_tier::classify("bash", Some(&serde_json::json!({ "command": cmd })))
    }

    /// PR #2028 review: every shape here is one where bash runs `sudo id`
    /// but a naive heredoc scanner would have dropped it as body text. Each
    /// must stay Privileged — never eligible for unattended approval.
    #[test]
    fn executable_text_is_never_dropped_as_a_body() {
        for cmd in [
            // Unquoted delimiter: `$(…)` and backticks run before cat sees them.
            "cat > f <<EOF\n$(sudo id)\nEOF",
            "cat > f <<EOF\n`sudo id`\nEOF",
            // `<<` inside a comment is not an operator; the next line runs.
            "cat /dev/null # <<EOF\nsudo id\nEOF",
            "cat /dev/null #<<EOF\nsudo id\nEOF",
            // Unquoted body: `E\<newline>OF` joins into the terminator.
            "cat > f <<EOF\nE\\\nOF\nsudo id\nEOF",
            // The terminator must match exactly; `  EOF` is still body, so a
            // later `cat <<'Z'` inside it must not open a fake heredoc.
            "cat > f <<'EOF'\n  EOF\ncat <<'Z'\nEOF\nsudo id\nZ",
            // A quoted delimiter may contain spaces: it is `E F`, not `E`.
            "cat > f <<'E F'\nE\ncat <<'Z'\nE F\nsudo id\nZ",
            // `$'\''` is one quote char: the `<<EOF` after it is quoted text.
            "echo $'\\'<<EOF'\ncat <<'Z'\nEOF\nsudo id\nZ\n'",
            // `<<` that is not a redirection at all.
            "echo ${x:-<<EOF}\nsudo id\nEOF",
            "echo $((1<<EOF))\nsudo id\nEOF",
            // `cat` that is not the data sink it looks like.
            "cat() { sh; }; cat <<'EOF'\nsudo id\nEOF",
            "function cat { sh; }\ncat <<'EOF'\nsudo id\nEOF",
            "shopt -s expand_aliases; alias cat=sh\ncat <<'EOF'\nsudo id\nEOF",
            "hash -p /bin/sh cat\ncat <<'EOF'\nsudo id\nEOF",
            "PATH=/tmp/x:$PATH cat <<'EOF'\nsudo id\nEOF",
            "./cat <<'EOF'\nsudo id\nEOF",
            // Output fed to a process substitution is executed.
            "cat <<'EOF' > >(sh)\nsudo id\nEOF",
            // Write-then-execute in the same command: the body WILL run.
            "cat > a.sh <<'EOF'\nsudo id\nEOF\nbash a.sh",
            "cat > Makefile <<'EOF'\nall:\n\tsudo id\nEOF\nmake",
        ] {
            let t = tier_of(cmd);
            assert_eq!(t, crate::hitl::RiskTier::Privileged, "cmd: {cmd:?}");
        }
    }

    /// The carriage return is part of bash's delimiter word (`'EOF'\r` →
    /// `EOF\r`), so `EOF\r` ends the body and the next line runs.
    #[test]
    fn a_crlf_delimiter_ends_where_bash_ends_it() {
        let s = scan_text("cat > f <<'EOF'\r\nlaunchctl x\r\nEOF\r\n");
        assert!(!s.contains("launchctl"), "{s:?}");
        let cmd = "cat > f <<'EOF'\r\nlaunchctl x\r\nEOF\r\nsudo id\nEOF";
        assert_eq!(tier_of(cmd), crate::hitl::RiskTier::Privileged);
    }

    /// What may still be dropped: literal bodies a comment or a plain
    /// unquoted delimiter does not change.
    #[test]
    fn literal_bodies_are_still_data() {
        for cmd in [
            "# write the fix\ncat > f <<'EOF' # quoted\nlaunchctl x\nEOF",
            "cat > f <<EOF\nlaunchctl x\nEOF",
            "cat > f <<\"EOF\" || true\nlaunchctl x\nEOF",
            "/bin/cat > f <<\\EOF\nlaunchctl $x\nEOF",
        ] {
            assert!(!scan_text(cmd).contains("launchctl"), "cmd: {cmd:?}");
        }
    }

    /// The other half: a heredoc fed to a shell IS executed and keeps its tier.
    #[test]
    fn a_heredoc_fed_to_a_shell_keeps_its_tier() {
        let cmd = "bash <<'EOF'\nlaunchctl bootout gui/501/x\nEOF";
        let t =
            crate::hitl::tool_tier::classify("bash", Some(&serde_json::json!({ "command": cmd })));
        assert!(!crate::hitl::tier_may_be_granted(t), "granted at {t:?}");
    }
}
