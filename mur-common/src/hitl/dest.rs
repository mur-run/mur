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
pub enum Destination {
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
pub struct Scope {
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
pub struct Word {
    pub text: String,
    pub quoted: bool,
}

/// What a tokenizer pass produced: words and the operators between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
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
pub fn tokenize(line: &str) -> Option<Vec<Token>> {
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
            // #1878: `2>/dev/null`, `>/dev/null`, `2>&1` only discard or join
            // the command's own streams; they cannot write a file. Skip them
            // whole. Every other redirect is still refused below.
            '>' | '1' | '2' if !have_cur => match harmless_redirect_len(&b[i..]) {
                Some(n) => i += n,
                None if c == '>' => return None,
                None => {
                    cur.push(c);
                    have_cur = true;
                    i += 1;
                }
            },
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

/// Length of a stream-discard/join redirect at the start of `rest`, or `None`.
///
/// Accepts exactly `[1|2]>/dev/null` and `[1|2]>&[1|2]`, optionally with
/// spaces after `>` before `/dev/null`, and only when followed by whitespace,
/// an operator, or the end of the line — so `2>/dev/nullx`, `2>/dev/null/x`
/// and `2>'/dev/null'` stay unparseable. `&>` and `>&-` never match.
fn harmless_redirect_len(rest: &[char]) -> Option<usize> {
    let mut i = usize::from(matches!(rest.first(), Some('1' | '2')));
    if rest.get(i) != Some(&'>') {
        return None;
    }
    i += 1;
    let tail_ok = |j: usize| {
        rest.get(j)
            .is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '\n'))
    };
    if rest.get(i) == Some(&'&') {
        return (matches!(rest.get(i + 1), Some('1' | '2')) && tail_ok(i + 2)).then_some(i + 2);
    }
    while rest.get(i).is_some_and(|c| *c == ' ') {
        i += 1;
    }
    let target: Vec<char> = DEV_NULL.chars().collect();
    let end = i + target.len();
    (rest.get(i..end) == Some(&target[..]) && tail_ok(end)).then_some(end)
}

/// The only file a redirect may name and still leave a read a read.
const DEV_NULL: &str = "/dev/null";

/// Group tokens into simple commands, dropping empty runs (`a ;; b`).
pub fn segments(tokens: Vec<Token>) -> Vec<Vec<Word>> {
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
pub fn segment_is_readonly(seg: &[Word]) -> bool {
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
            // Delegate to the audited list, carrying the quoting through: a
            // metacharacter inside a quoted word is data this pass already
            // unquoted, not an operator, and re-checking the re-joined text
            // would reject it.
            let words: Vec<(&str, bool)> =
                seg.iter().map(|w| (w.text.as_str(), w.quoted)).collect();
            bash_class::is_readonly_words(&words)
        }
    }
}

/// Classify a whole `bash` command into a grantable scope, or `None`.
///
/// `None` means "not grantable" and NEVER means "safe": every caller must
/// treat it as an ordinary prompt.
pub fn classify_bash(cmd: &str) -> Option<Scope> {
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

#[cfg(test)]
mod tests;
