//! Destination-scoped read-only classification of a `bash` command.
//!
//! ## Why this exists
//!
//! The session grant is keyed on the TOOL NAME (`session_tool_allow`), and the
//! ceiling that bounds it (`tier_may_be_granted`) stops at `Write`. Together
//! those two facts make "don't ask again for `bash`" a lie for the one workflow
//! that needs it most: remote diagnosis. Every `ssh host tail -n 120 log`
//! classifies as `NetworkEgress`, `tier_may_be_granted` returns false, the
//! session grant is never consulted, and the operator is asked again — with no
//! feedback explaining why the answer they already gave did not stick.
//!
//! The grant was also the wrong SHAPE. `bash` is not a blast radius; a host is.
//! "I trust reading on `karajan@people.example.edu`" is a sentence an operator
//! can actually mean, and can actually audit. "I trust `bash`" is not.
//!
//! So a grant here is keyed on a DESTINATION plus a read-only proof:
//!
//! ```text
//! ssh:karajan@people.example.edu:ro     ← reads on that host, this session
//! local:ro                              ← reads on this machine, this session
//! ```
//!
//! Nothing that writes is ever grantable, at either end. A write still asks
//! every time, which is the whole reason the read grant can be this wide.
//!
//! ## What is proved, and what is not
//!
//! Same posture as [`super::bash_class`]: this is a deny-list over command
//! heads wrapped in a quote-aware parser, and a deny-list is only as good as
//! its last audit. Every uncertainty returns `None` (→ an ordinary prompt).
//! In particular:
//!
//! - **Unknown head ⇒ not read-only.** Never the other way round.
//! - **Substitution is fatal.** A backtick or `$(…)` anywhere outside single
//!   quotes ends the classification: its output is a command this parser
//!   cannot see.
//! - **Redirection is fatal.** `>`, `>>` and `<` all mean bytes move somewhere
//!   this parser is not modelling. Heredocs are the one exception, because
//!   they are parsed (below) rather than guessed at.
//! - **One hop only.** An `ssh` inside an `ssh` payload is a second
//!   destination; the human named one. Two hops ⇒ `None`.
//! - **One destination per command.** `ssh a … && ssh b …` is not grantable
//!   under either host's key.
//!
//! ## Heredocs, recursively
//!
//! The command that provoked this module looks like:
//!
//! ```text
//! ssh -o BatchMode=yes user@host 'sh -s' <<'REMOTE'
//! cd /srv/app || exit 1
//! echo --TAIL--; tail -n 120 storage/logs/app.log
//! REMOTE
//! ```
//!
//! The interesting half is inside the heredoc, so a parser that stops at the
//! `<<` has classified nothing. [`strip_heredocs`] lifts each body out and
//! leaves a placeholder token in its place, which keeps the body bound to the
//! exact segment that redirected it. Each body is then classified by the same
//! entry point that classified the outer line — so a heredoc inside a heredoc
//! is handled by construction, not by a second code path.
//!
//! A heredoc whose tag is UNQUOTED (`<<EOF`, not `<<'EOF'`) is expanded by the
//! local shell before it is sent. If such a body contains `$` or a backtick,
//! the expansion is local execution this parser cannot see, and the whole
//! command is refused.

use std::fmt;

use super::bash_class;

/// Where a command's effects land.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Destination {
    /// This machine.
    Local,
    /// One `ssh` hop, stored as the operator wrote it (`[user@]host`),
    /// lowercased so `Host` and `host` cannot buy two separate grants.
    Remote(String),
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => f.write_str("this machine"),
            Self::Remote(h) => f.write_str(h),
        }
    }
}

/// A grantable scope: one destination, proved read-only there.
///
/// Constructed ONLY by [`classify`], and only when the proof succeeded — there
/// is deliberately no way to build a writing scope, because a writing scope is
/// not a thing a session grant may hold.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Scope {
    pub dest: Destination,
}

impl Scope {
    /// The session-grant key. Carries `:ro` even though every scope is
    /// read-only today, so that a future writing scope cannot silently inherit
    /// a key an operator granted for reads.
    pub fn key(&self) -> String {
        match &self.dest {
            Destination::Local => "local:ro".to_string(),
            Destination::Remote(h) => format!("ssh:{h}:ro"),
        }
    }

    /// The menu-row wording. Says the destination and the limit, because a
    /// grant an operator cannot restate is a grant they cannot audit.
    pub fn label(&self) -> String {
        match &self.dest {
            Destination::Local => {
                "Yes, and don't ask again for read-only commands on this machine this session"
                    .to_string()
            }
            Destination::Remote(h) => {
                format!("Yes, and don't ask again for read-only commands on `{h}` this session")
            }
        }
    }
}

/// Heads that read, on top of [`bash_class`]'s audited list, and that a remote
/// diagnosis session cannot do without.
///
/// DELIBERATELY ABSENT — do not add without reading why:
///   `sed`  — `-i` edits in place and a `w` command writes a file; telling
///            those apart from `/warn/p` needs a sed parser, not a guard.
///   `sort` — `-o FILE` overwrites (already noted in `bash_class`).
///   `env`  — execs its argv, so it is arbitrary execution wearing a read's
///            clothes (already noted in `bash_class`).
///   `php`/`python`/`perl` — arbitrary programs. `php artisan tinker` is the
///            exact call this module must keep asking about.
const EXTRA_READONLY_HEADS: &[&str] = &[
    // `exit` and `cd` change the shell's own state, not any bytes — and both
    // are unavoidable in a remote script (`cd /srv/app || exit 1`).
    "cd", "exit", "ps", "uptime", "free", "id", "groups", "vmstat", "iostat", "lsof", "netstat",
    "ss", "last",
];

/// `awk` reads UNLESS its program writes. `print > "f"`, `printf >> "f"` and
/// `system("…")` all escape, and all of them need a character this test can
/// look for. Checked against the WHOLE segment, so a redirect hidden inside
/// the quoted program is still seen.
const AWK_WRITE_MARKERS: &[&str] = &["system(", ">", "close(", "|&", "ENVIRON["];

/// One parsed token. `quoted` records whether the operator wrapped it, which
/// is what distinguishes `ssh host 'cd /x && tail f'` (one remote payload)
/// from `ssh host cd /x && tail f` (a local `tail` after the hop).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Word {
    text: String,
    quoted: bool,
}

/// What a tokenizer pass produced: words and the operators between them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(Word),
    /// A separator that ends one simple command: `;` `&&` `||` `|` or newline.
    Sep,
}

/// Placeholder left in the line where a heredoc body was lifted out. Uses
/// control characters so no real command can collide with it.
fn heredoc_marker(i: usize) -> String {
    format!("\u{1}HEREDOC{i}\u{1}")
}

/// A heredoc body lifted out of a command line.
struct Heredoc {
    body: String,
    /// `<<'TAG'` / `<<"TAG"` — the body is sent literally. An unquoted tag is
    /// expanded locally first, which is a different (and more dangerous) thing.
    tag_quoted: bool,
}

/// Lift every heredoc body out of `cmd`, leaving a placeholder word where the
/// `<<TAG` operator stood.
///
/// The placeholder matters: it keeps each body attached to the segment that
/// redirected it, so `a <<X` and `b <<Y` cannot have their bodies swapped by a
/// later pass. Returns `None` if a heredoc is opened and never terminated —
/// an unterminated body is content this parser never sees.
fn strip_heredocs(cmd: &str) -> Option<(String, Vec<Heredoc>)> {
    let mut line = String::new();
    let mut docs: Vec<Heredoc> = Vec::new();
    let mut rest = cmd.to_string();
    loop {
        let Some(pos) = find_heredoc_op(&rest) else {
            line.push_str(&rest);
            return Some((line, docs));
        };
        line.push_str(&rest[..pos]);
        // Past the `<<`, and past a `-` (tab-stripping form).
        let after = &rest[pos + 2..];
        let after = after.strip_prefix('-').unwrap_or(after);
        let after = after.trim_start_matches(' ');
        let (tag, tag_quoted, consumed) = read_heredoc_tag(after)?;
        let body_start = &after[consumed..];
        // The body begins on the NEXT line.
        let nl = body_start.find('\n')?;
        let (head_tail, body_and_rest) = body_start.split_at(nl + 1);
        let (body, remainder) = split_at_tag(body_and_rest, &tag)?;
        line.push_str(&heredoc_marker(docs.len()));
        line.push_str(head_tail);
        docs.push(Heredoc { body, tag_quoted });
        rest = remainder;
    }
}

/// Byte offset of the next `<<` that is a heredoc operator — outside quotes,
/// and not the `<<<` here-string (whose word is data, not a body).
fn find_heredoc_op(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'\'' || c == b'"' {
                    quote = Some(c);
                } else if c == b'<' && b.get(i + 1) == Some(&b'<') {
                    if b.get(i + 2) == Some(&b'<') {
                        return None; // here-string: not a body, refuse later
                    }
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Read the heredoc tag at the start of `s`. Returns the tag, whether it was
/// quoted, and how many bytes it occupied.
fn read_heredoc_tag(s: &str) -> Option<(String, bool, usize)> {
    let b = s.as_bytes();
    let first = *b.first()?;
    if first == b'\'' || first == b'"' {
        let end = s[1..].find(first as char)? + 1;
        return Some((s[1..end].to_string(), true, end + 1));
    }
    let end = s
        .find(|c: char| c.is_whitespace() || c == ';' || c == '&' || c == '|')
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((s[..end].to_string(), false, end))
}

/// Split a heredoc body at its terminator line. The terminator is a line that
/// is exactly the tag (after trimming, which covers the `<<-` form).
fn split_at_tag(s: &str, tag: &str) -> Option<(String, String)> {
    let mut offset = 0usize;
    for line in s.split_inclusive('\n') {
        if line.trim() == tag {
            let body = s[..offset].to_string();
            let rest = s[offset + line.len()..].to_string();
            return Some((body, rest));
        }
        offset += line.len();
    }
    None
}

/// Tokenize one heredoc-free command line.
///
/// Returns `None` the moment it sees something whose behaviour it cannot
/// model: command substitution, any redirection, a background `&`, or a
/// subshell. Each of those is a place where bytes or control go somewhere the
/// segment list does not describe.
fn tokenize(line: &str) -> Option<Vec<Token>> {
    let mut out: Vec<Token> = Vec::new();
    let mut cur = String::new();
    let mut cur_quoted = false;
    let mut have_cur = false;
    let b: Vec<char> = line.chars().collect();
    let mut i = 0usize;

    macro_rules! flush {
        () => {
            if have_cur {
                out.push(Token::Word(Word {
                    text: std::mem::take(&mut cur),
                    quoted: cur_quoted,
                }));
                #[allow(unused_assignments)]
                {
                    cur_quoted = false;
                    have_cur = false;
                }
            }
        };
    }

    while i < b.len() {
        let c = b[i];
        match c {
            '\'' => {
                let end = b[i + 1..].iter().position(|&x| x == '\'')? + i + 1;
                cur.push_str(&b[i + 1..end].iter().collect::<String>());
                cur_quoted = true;
                have_cur = true;
                i = end + 1;
            }
            '"' => {
                let end = b[i + 1..].iter().position(|&x| x == '"')? + i + 1;
                let inner: String = b[i + 1..end].iter().collect();
                // A double-quoted string still expands `$(…)` and backticks.
                if inner.contains('`') || inner.contains("$(") {
                    return None;
                }
                cur.push_str(&inner);
                cur_quoted = true;
                have_cur = true;
                i = end + 1;
            }
            // Substitution: the output is a command we cannot see.
            '`' => return None,
            '$' if b.get(i + 1) == Some(&'(') => return None,
            // Redirection of any kind (heredocs were lifted out already).
            '>' | '<' => return None,
            // Subshells and process substitution.
            '(' | ')' => return None,
            ';' | '\n' => {
                flush!();
                out.push(Token::Sep);
                i += 1;
            }
            '&' => {
                if b.get(i + 1) == Some(&'&') {
                    flush!();
                    out.push(Token::Sep);
                    i += 2;
                } else {
                    return None; // background: the command outlives the gate
                }
            }
            '|' => {
                flush!();
                out.push(Token::Sep);
                i += if b.get(i + 1) == Some(&'|') { 2 } else { 1 };
            }
            c if c.is_whitespace() => {
                flush!();
                i += 1;
            }
            c => {
                cur.push(c);
                have_cur = true;
                i += 1;
            }
        }
    }
    flush!();
    Some(out)
}

/// Group tokens into simple commands, dropping empty runs (`a ;; b`).
fn segments(tokens: Vec<Token>) -> Vec<Vec<Word>> {
    let mut out: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    for t in tokens {
        match t {
            Token::Sep => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            Token::Word(w) => cur.push(w),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `ssh` flags that consume the NEXT argument, so the target host is not
/// mistaken for an option's value.
const SSH_FLAGS_WITH_VALUE: &[&str] = &[
    "-o", "-p", "-i", "-l", "-F", "-c", "-m", "-b", "-D", "-L", "-R", "-W", "-J", "-Q", "-E", "-S",
];

/// Is this one simple command a read, ignoring where it runs?
fn segment_is_readonly(seg: &[Word]) -> bool {
    let Some(head) = seg.first() else {
        return false;
    };
    let whole = seg
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    match head.text.as_str() {
        // `awk` reads unless its program writes or shells out.
        "awk" | "gawk" | "mawk" => !AWK_WRITE_MARKERS.iter().any(|m| whole.contains(m)),
        // `cd` moves the cursor, it does not touch bytes.
        "cd" => true,
        other => {
            if EXTRA_READONLY_HEADS.contains(&other) {
                return true;
            }
            // Delegate to the audited list. `is_readonly_bash` re-runs its own
            // metacharacter guard, which is harmless here: this segment has
            // already been split on every operator that guard rejects.
            bash_class::is_readonly_bash(&whole)
        }
    }
}

/// Classify a whole `bash` command into a grantable scope, or `None`.
///
/// `None` means "not grantable" and NEVER means "safe": every caller must
/// treat it as an ordinary prompt.
pub(super) fn classify_bash(cmd: &str) -> Option<Scope> {
    classify_inner(cmd, 0)
}

/// `depth` counts `ssh` hops. One is the most a grant may describe, because
/// the operator named one host.
fn classify_inner(cmd: &str, depth: u8) -> Option<Scope> {
    if depth > 1 {
        return None;
    }
    let (line, docs) = strip_heredocs(cmd)?;
    let segs = segments(tokenize(&line)?);
    if segs.is_empty() {
        return None;
    }
    let mut dest: Option<Destination> = None;

    for seg in &segs {
        let seg_dest = if seg[0].text == "ssh" {
            let (host, payload) = ssh_target_and_payload(seg, &docs)?;
            // The payload must itself be a read, at the far end. Recursing
            // through the same entry point is what makes a heredoc inside a
            // heredoc work without a second parser.
            let inner = classify_inner(&payload, depth + 1)?;
            if inner.dest != Destination::Local {
                return None; // a second hop; the human named one host
            }
            Destination::Remote(host)
        } else {
            if !segment_is_readonly(seg) {
                return None;
            }
            // A placeholder surviving on a non-ssh segment means a heredoc fed
            // some other command — content this pass did not classify.
            if seg.iter().any(|w| w.text.contains('\u{1}')) {
                return None;
            }
            Destination::Local
        };
        match (&dest, &seg_dest) {
            (None, _) => dest = Some(seg_dest),
            // Local setup around one remote hop keeps the remote scope.
            (Some(Destination::Local), Destination::Remote(_)) => dest = Some(seg_dest),
            (Some(Destination::Remote(_)), Destination::Local) => {}
            (Some(a), b) if a == b => {}
            // Two different hosts in one command: grantable under neither.
            _ => return None,
        }
    }
    dest.map(|dest| Scope { dest })
}

/// Pull the `[user@]host` and the remote payload out of one `ssh` segment.
///
/// The payload is whatever follows the target: either arguments (`ssh h tail
/// -n 5 f`), a single quoted script (`ssh h 'a && b'`), or a heredoc fed to a
/// stdin shell (`ssh h sh -s <<'EOF'`). The three produce the same thing — a
/// string to be classified by the same rules as any other command line.
fn ssh_target_and_payload(seg: &[Word], docs: &[Heredoc]) -> Option<(String, String)> {
    let mut i = 1usize;
    while i < seg.len() {
        let t = seg[i].text.as_str();
        if SSH_FLAGS_WITH_VALUE.contains(&t) {
            i += 2;
            continue;
        }
        if t.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    let target = seg.get(i)?.text.to_ascii_lowercase();
    // A bare `-` or an option value that slipped through is not a host.
    if target.is_empty() || target.starts_with('-') {
        return None;
    }
    let rest = &seg[i + 1..];

    // Any heredoc placeholder in the remaining words IS the payload: the
    // command words are then just the stdin shell that consumes it.
    for w in rest {
        if let Some(body) = heredoc_for(&w.text, docs) {
            return Some((target, body?));
        }
    }
    if rest.is_empty() {
        // `ssh host` with no command is an interactive login, not a read.
        return None;
    }
    Some((
        target,
        rest.iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    ))
}

/// Resolve a placeholder word to its body. The outer `Option` is "was this a
/// placeholder at all"; the inner one is "is the body usable" — an unquoted
/// tag whose body would be expanded locally is not.
fn heredoc_for(word: &str, docs: &[Heredoc]) -> Option<Option<String>> {
    if !word.contains('\u{1}') {
        return None;
    }
    for (i, d) in docs.iter().enumerate() {
        if word.contains(&heredoc_marker(i)) {
            if !d.tag_quoted && (d.body.contains('$') || d.body.contains('`')) {
                return Some(None);
            }
            return Some(Some(d.body.clone()));
        }
    }
    Some(None)
}

/// What "don't ask again" would actually buy for one gated call.
///
/// The modal used to offer that row unconditionally and key it on the tool
/// name, which produced the bug this module exists for: the operator picked
/// it, `session_tool_allow` recorded `bash`, and the next call was asked
/// anyway because `tier_may_be_granted(NetworkEgress)` is false and the set
/// was never consulted. The intent was dropped in silence.
///
/// So the row's meaning is computed ONCE, here, and both the menu (which
/// prints it) and the auto lane (which honours it) read the same answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Grant {
    /// Grantable, keyed on a destination proved read-only.
    Scoped(Scope),
    /// Grantable, keyed on the tool name — the pre-existing behaviour, still
    /// bounded by `tier_may_be_granted`.
    Tool(String),
    /// NOT grantable. Carries the reason so the row can say it out loud
    /// instead of pretending.
    Refused(&'static str),
}

impl Grant {
    /// The `session_tool_allow` key, or `None` when nothing may be stored.
    pub fn key(&self) -> Option<String> {
        match self {
            Self::Scoped(s) => Some(s.key()),
            Self::Tool(t) => Some(t.clone()),
            Self::Refused(_) => None,
        }
    }

    /// The menu-row wording.
    pub fn label(&self) -> String {
        match self {
            Self::Scoped(s) => s.label(),
            Self::Tool(t) => format!("Yes, and don't ask again for `{t}` this session"),
            Self::Refused(why) => format!("Yes (can't skip future asks — {why})"),
        }
    }
}

/// Decide what a session grant may cover for this call.
///
/// The order matters. A destination scope is tried FIRST, because its proof is
/// stronger than the tier check it would otherwise fall to: `ssh host tail -f
/// log` is `NetworkEgress` by head, but a parsed, one-hop, read-only-at-both-
/// ends command is a narrower thing than that tier describes, and refusing to
/// remember it is what trained the operator to answer without reading.
pub(super) fn grant_for(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    tier: mur_common::hitl::RiskTier,
) -> Grant {
    if tool_name == "bash"
        && let Some(cmd) = tool_input
            .and_then(|v| v.get("command"))
            .and_then(serde_json::Value::as_str)
    {
        if let Some(scope) = classify_bash(cmd) {
            return Grant::Scoped(scope);
        }
        if !mur_common::hitl::tier_may_be_granted(tier) {
            return Grant::Refused(refusal_reason(tier));
        }
    }
    if mur_common::hitl::tier_may_be_granted(tier) {
        return Grant::Tool(tool_name.to_string());
    }
    Grant::Refused("this tier always asks")
}

/// Why a tier above the ceiling cannot be remembered, in words that name
/// that tier. It used to be one sentence for all four — "this command writes
/// or leaves the machine" — which told the operator a `sudo` could leak data
/// and a `git push` could delete it, and so told them nothing.
fn refusal_reason(tier: mur_common::hitl::RiskTier) -> &'static str {
    use mur_common::hitl::RiskTier;
    match tier {
        RiskTier::NetworkEgress => "this command sends data off this machine",
        RiskTier::Destructive => "this command can delete or overwrite data",
        RiskTier::Privileged => "this command runs with elevated privileges",
        RiskTier::Spend => "this command can spend money",
        // Unreachable behind `tier_may_be_granted`, but a wording must exist.
        RiskTier::Read | RiskTier::Write => "this tier always asks",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the grant row promises must be what the auto lane honours. These
    /// assert the JOIN between `grant_for` (which words the row and stores the
    /// key) and `stream_handler` (which looks the key up) — the seam where the
    /// original bug lived: the row said "don't ask again for `bash`", the set
    /// recorded `bash`, and the lookup never ran because the tier was above
    /// the ceiling.
    mod grants {
        use super::*;
        use mur_common::hitl::RiskTier;

        fn bash(cmd: &str) -> serde_json::Value {
            serde_json::json!({ "command": cmd })
        }

        /// The exact call from the report. `ssh` is `NetworkEgress`, so the
        /// old code offered a grant that could never fire; now the row is
        /// keyed on the proved destination and the key is real.
        #[test]
        fn a_remote_read_is_granted_on_its_destination_not_on_bash() {
            let cmd = "ssh -o BatchMode=yes karajan@people.example.edu 'tail -n 120 app.log'";
            let g = grant_for("bash", Some(&bash(cmd)), RiskTier::NetworkEgress);
            assert_eq!(
                g.key().as_deref(),
                Some("ssh:karajan@people.example.edu:ro"),
                "a proved remote read must be grantable despite its tier"
            );
            assert!(
                g.label().contains("karajan@people.example.edu"),
                "the row must name the host it is about to trust: {}",
                g.label()
            );
        }

        /// A grant is never offered as a promise it cannot keep. The row says
        /// so in words, and `key()` returns nothing to store.
        #[test]
        fn an_ungrantable_call_says_so_instead_of_pretending() {
            for cmd in [
                "ssh karajan@people.example.edu 'php artisan queue:restart'",
                "ssh karajan@people.example.edu 'rm -rf /tmp/x'",
            ] {
                let g = grant_for("bash", Some(&bash(cmd)), RiskTier::NetworkEgress);
                assert_eq!(g.key(), None, "must not store a grant for: {cmd}");
                assert!(
                    g.label().contains("can't skip future asks"),
                    "the row must admit it won't stick: {}",
                    g.label()
                );
            }
        }

        /// Every read shape in one diagnosis session collapses to ONE key, so
        /// a single answer covers the whole session. This is the actual UX
        /// fix: seven prompts become one.
        #[test]
        fn one_answer_covers_a_whole_diagnosis_session() {
            let session = [
                "ssh -o BatchMode=yes karajan@people.example.edu 'sh -s' <<'REMOTE'\ncd /home/web/app || exit 1\ntail -n 120 storage/logs/app.log\nREMOTE\n",
                "ssh -o BatchMode=yes karajan@people.example.edu 'cd /home/web/app && echo --TAIL--; tail -n 120 log'",
                "ssh karajan@people.example.edu sh -s <<'REMOTE'\ncd /home/web/app || exit 1\nps aux | grep php\nREMOTE\n",
                "ssh karajan@people.example.edu \"awk '/2026-09-21/ && /PayslipSend/ {n++} END{print n}' log\"",
                "ssh -p 2222 karajan@people.example.edu tail -n 50 /var/log/syslog",
            ];
            let granted = grant_for("bash", Some(&bash(session[0])), RiskTier::NetworkEgress)
                .key()
                .expect("the first call must be grantable");
            for cmd in &session[1..] {
                assert_eq!(
                    grant_for("bash", Some(&bash(cmd)), RiskTier::NetworkEgress).key(),
                    Some(granted.clone()),
                    "should have been covered by the first answer: {cmd}"
                );
            }
        }

        /// The refusal names the tier it refused, not a catch-all.
        #[test]
        fn a_refusal_names_its_own_tier() {
            let why = |cmd: &str, t| grant_for("bash", Some(&bash(cmd)), t).label();
            assert!(why("git push", RiskTier::NetworkEgress).contains("sends data off"));
            assert!(why("rm -rf x", RiskTier::Destructive).contains("delete or overwrite"));
            assert!(why("sudo ls", RiskTier::Privileged).contains("elevated"));
        }

        /// A tool with no command still falls back to the old tool-name grant,
        /// so nothing that worked before this module stops working.
        #[test]
        fn non_bash_tools_keep_the_tool_name_grant() {
            assert_eq!(
                grant_for("write_file", None, RiskTier::Write)
                    .key()
                    .as_deref(),
                Some("write_file")
            );
            assert_eq!(
                grant_for("fleet_run", None, RiskTier::Spend).key(),
                None,
                "a Spend tool was never grantable and still is not"
            );
        }
    }

    fn scope(cmd: &str) -> Option<String> {
        classify_bash(cmd).map(|s| s.key())
    }

    /// The command that opened the issue: a heredoc'd read script fed to a
    /// stdin shell on a remote host. Every one of these asked again, every
    /// time, no matter what the operator had already answered.
    #[test]
    fn heredoc_read_script_over_ssh_is_grantable() {
        let cmd = "ssh -o BatchMode=yes karajan@people.example.edu 'sh -s' <<'REMOTE'\ncd /home/web/app || exit 1\necho --TAIL--\ntail -n 120 storage/logs/app.log\nREMOTE\n";
        assert_eq!(
            scope(cmd).as_deref(),
            Some("ssh:karajan@people.example.edu:ro")
        );
    }

    /// The other three shapes the same session produced, which must all land
    /// on the SAME key — otherwise "don't ask again" still asks.
    #[test]
    fn every_read_shape_to_one_host_shares_one_key() {
        let want = Some("ssh:karajan@people.example.edu:ro");
        for cmd in [
            "ssh -o BatchMode=yes karajan@people.example.edu 'cd /home/web/app && echo --TAIL--; tail -n 120 log'",
            "ssh karajan@people.example.edu \"cd /home/web/app && ls -la\"",
            "ssh -p 2222 karajan@people.example.edu tail -n 50 /var/log/syslog",
            "ssh karajan@people.example.edu sh -s <<'REMOTE'\nps aux | grep php\nREMOTE\n",
        ] {
            assert_eq!(scope(cmd).as_deref(), want, "cmd: {cmd}");
        }
    }

    /// `awk` with the operator's real filter — the `&&` lives inside single
    /// quotes and must not be read as a shell operator.
    #[test]
    fn quoted_operators_are_not_shell_operators() {
        let cmd = "ssh karajan@people.example.edu \"awk '/2026-09-21/ && /PayslipSend/ {n++} END{print n}' storage/logs/app.log\"";
        assert_eq!(
            scope(cmd).as_deref(),
            Some("ssh:karajan@people.example.edu:ro")
        );
    }

    /// Writes ask, at either end — the promise that lets the read grant be
    /// this wide. Each of these appeared in the same diagnosis session.
    #[test]
    fn writes_are_never_grantable() {
        for cmd in [
            "ssh karajan@people.example.edu 'cat >/tmp/status.php'",
            "ssh karajan@people.example.edu 'php artisan tinker --execute=\"...\"'",
            "ssh karajan@people.example.edu 'rm -rf /tmp/x'",
            "ssh karajan@people.example.edu sudo systemctl restart php-fpm",
            "ssh karajan@people.example.edu \"awk '{print > \\\"/tmp/out\\\"}' f\"",
            "tail -n 5 log > /tmp/copy",
        ] {
            assert_eq!(scope(cmd), None, "must still ask: {cmd}");
        }
    }

    /// Substitution hides a command from this parser, so it ends the proof
    /// even when every visible head reads.
    #[test]
    fn substitution_refuses_the_whole_command() {
        for cmd in [
            "ssh karajan@people.example.edu \"tail -n 5 $(cat /tmp/which)\"",
            "ssh karajan@people.example.edu \"cat `cat /tmp/which`\"",
            "cat /tmp/$(whoami)",
        ] {
            assert_eq!(scope(cmd), None, "must still ask: {cmd}");
        }
    }

    /// One grant, one host. A command spanning two hosts belongs to neither,
    /// and a second hop is a destination the operator never saw.
    #[test]
    fn a_grant_covers_exactly_one_destination() {
        assert_eq!(
            scope("ssh a@one.example 'ls' && ssh b@two.example 'ls'"),
            None,
            "two hosts in one command must not ride either host's grant"
        );
        assert_eq!(
            scope("ssh a@one.example 'ssh b@two.example ls'"),
            None,
            "a second hop is a destination the human never named"
        );
    }

    /// An unquoted heredoc tag is expanded by the LOCAL shell first, so a `$`
    /// in the body is local execution wearing a remote command's clothes.
    #[test]
    fn unquoted_heredoc_tag_with_expansion_is_refused() {
        let expanded =
            "ssh karajan@people.example.edu sh -s <<REMOTE\ntail -n 5 $(whoami).log\nREMOTE\n";
        assert_eq!(scope(expanded), None);
        // The same body with nothing to expand stays grantable.
        let literal = "ssh karajan@people.example.edu sh -s <<REMOTE\ntail -n 5 app.log\nREMOTE\n";
        assert_eq!(
            scope(literal).as_deref(),
            Some("ssh:karajan@people.example.edu:ro")
        );
    }

    /// Local reads get a scope too, so the same mechanism answers the local
    /// half of a session instead of a second, differently-shaped grant.
    #[test]
    fn local_reads_share_one_local_key() {
        for cmd in [
            "ls -la src/",
            "cat Cargo.toml",
            "git status",
            "cd /tmp && ls",
        ] {
            assert_eq!(scope(cmd).as_deref(), Some("local:ro"), "cmd: {cmd}");
        }
    }

    /// Local setup around one hop keeps the REMOTE key: the thing being
    /// trusted is the host, and an `echo` before it does not change that.
    #[test]
    fn local_preamble_keeps_the_remote_key() {
        assert_eq!(
            scope("echo checking; ssh karajan@people.example.edu 'ls -la'").as_deref(),
            Some("ssh:karajan@people.example.edu:ro")
        );
    }

    /// An interactive login runs whatever the operator types next, which is
    /// not a command this gate ever saw.
    #[test]
    fn a_bare_login_is_not_a_read() {
        assert_eq!(scope("ssh karajan@people.example.edu"), None);
    }

    /// An unterminated heredoc means the body was never seen. Fail closed.
    #[test]
    fn unterminated_heredoc_is_refused() {
        assert_eq!(scope("ssh h 'sh -s' <<'EOF'\nls\n"), None);
    }

    /// Case cannot buy two grants for one host.
    #[test]
    fn host_case_is_normalised() {
        assert_eq!(
            scope("ssh Karajan@People.Example.EDU ls").as_deref(),
            Some("ssh:karajan@people.example.edu:ro")
        );
    }
}
