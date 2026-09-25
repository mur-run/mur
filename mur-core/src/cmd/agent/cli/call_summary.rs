//! One-line summaries of a tool call: the arg hint on a transcript card, and
//! the `bash · git push …` line an approval decision is recorded under.
//!
//! Split out of `render_card.rs` for CLAUDE.md §4's 800-line rule. The hint
//! helpers moved verbatim; `approval_summary` is the new part, and it lives
//! here because the approval line and the card hint must elide the same
//! command the same way or the operator sees two different strings for one
//! call.

use serde_json::Value;

use super::step::StepCard;

/// Columns the header spends on everything that is not the arg hint: the state
/// glyph, the tool name, the separators and the right-hand result gist.
const HEADER_OVERHEAD: usize = 40;

/// Floor for the arg hint on a narrow terminal — below this the header would be
/// an ellipsis with barely any command in front of it, which says nothing.
pub(super) const ARG_HINT_MIN: usize = 24;

/// Bytes of arg hint that fit on one header row at `width` columns.
///
/// Was a flat 40 regardless of terminal size: on a 120-column terminal the
/// command was cut two thirds of the way short with the rest of the row left
/// empty, and on a narrow one the header still wrapped. An 80-column terminal
/// lands back on the old 40 — this widens the wide case, it does not re-tune
/// the common one.
pub(super) fn hint_budget(width: u16) -> usize {
    usize::from(width)
        .saturating_sub(HEADER_OVERHEAD)
        .max(ARG_HINT_MIN)
}

/// One display column carved out of the hint budget for the `…`
/// middle-elision marker itself, so `elide_middle` never returns something
/// longer than `budget` columns. The budget arithmetic here is byte-based,
/// not column-based, so this (like the rest of the budget) is only an
/// approximation of display width once the string contains wide characters.
const ELLIPSIS_OVERHEAD: usize = 1;

/// Session-constant prefix every bash command shares when the agent re-`cd`s
/// into the working directory before each call. Stripping it before hinting
/// is what stops eighteen rows in a row from sharing one useless 55-byte
/// prefix and differing only in the part that gets truncated away.
pub(super) fn strip_cd_prefix(cmd: &str) -> &str {
    let Some(rest) = cmd.strip_prefix("cd ") else {
        return cmd;
    };
    match rest.find(" && ") {
        Some(i) => &rest[i + 4..],
        None => cmd,
    }
}

/// Pick the field that actually describes what a tool call did: the command
/// for bash, the path for file tools, falling back to the first string value
/// in `args` (the old behaviour, which picked an arbitrary key because
/// `serde_json::Map` sorts alphabetically) only when neither is present.
pub(super) fn hint_field_of<'a>(tool: &str, args: &'a Value) -> Option<&'a str> {
    let obj = args.as_object()?;
    if tool.eq_ignore_ascii_case("bash")
        && let Some(cmd) = obj.get("command").and_then(|v| v.as_str())
    {
        return Some(cmd);
    }
    if let Some(path) = obj
        .get("file_path")
        .or_else(|| obj.get("path"))
        .and_then(|v| v.as_str())
    {
        return Some(path);
    }
    obj.values().find_map(|v| v.as_str())
}

pub(super) fn hint_field(card: &StepCard) -> Option<&str> {
    hint_field_of(&card.name, &card.args)
}

/// Keep the HEAD, cut at a word boundary.
///
/// For a shell command the identifying part is the front — the program and its
/// first arguments — and the tail is routinely a branch that did not run.
/// Middle-elision at 80 columns turned
/// `grep -m1 '^version' Cargo.toml 2>/dev/null || head -20 Cargo.toml`
/// into `grep -m1 '^version'… head -20 Cargo.toml`, which reads as though the
/// fallback executed. At 60 it produced `grep -m1 '^…0 Cargo.toml`.
///
/// Cutting at the last space keeps the hint from ending mid-token, unless that
/// would throw away most of the budget on a single long word.
pub(super) fn elide_tail_at_word(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let keep = budget.saturating_sub(ELLIPSIS_OVERHEAD);
    let end = s.floor_char_boundary(keep);
    let cut = match s[..end].rfind(' ') {
        // Backing up past half the budget costs more than the ragged edge.
        Some(i) if i * 2 >= end => i,
        _ => end,
    };
    format!("{}…", s[..cut].trim_end())
}

/// Truncate `s` to `budget` bytes by cutting out of the middle and keeping
/// head and tail, so the distinguishing suffix (a filename, a flag, the part
/// that differs between two otherwise-identical commands) survives instead
/// of being the first thing cut. Uses `floor_char_boundary`/
/// `ceil_char_boundary` so multi-byte chars (CJK, emoji) can't be split.
pub(super) fn elide_middle(s: &str, budget: usize) -> String {
    if s.len() <= budget {
        return s.to_string();
    }
    let keep = budget.saturating_sub(ELLIPSIS_OVERHEAD);
    let head_len = keep / 2;
    let tail_len = keep - head_len;
    let head_end = s.floor_char_boundary(head_len);
    let tail_start = s.ceil_char_boundary(s.len().saturating_sub(tail_len));
    if tail_start <= head_end {
        // Budget too small to fit both a head and a tail — fall back to a
        // plain head clip rather than emit an empty or malformed hint.
        let end = s.floor_char_boundary(budget);
        return format!("{}…", &s[..end]);
    }
    format!("{}…{}", &s[..head_end], &s[tail_start..])
}

/// The model's own one-line intent for this call, when it supplied one.
///
/// Model-generated, so it can drift from what actually executes: it is never
/// the approval target, only context beside the command.
pub(super) fn tool_description(card: &StepCard) -> Option<String> {
    description_of(&card.args)
}

/// Same field, read straight off a raw tool-input object — the approval path
/// has a `Value`, not a built card.
pub(super) fn description_of(input: &Value) -> Option<String> {
    let d = input.as_object()?.get("description")?.as_str()?.trim();
    (!d.is_empty()).then(|| d.to_string())
}

/// Split a summary from [`approval_summary`] into command and trailing intent
/// note, so a caller that styles spans can dim the intent independently.
///
/// The intent is the last parenthetical and only when it closes the string — a
/// `(` inside the command itself stays part of the command.
pub(super) fn split_intent(summary: &str) -> (&str, &str) {
    match summary.rfind(INTENT_LEAD) {
        Some(i) if summary.ends_with(')') => (&summary[..i], &summary[i..]),
        _ => (summary, ""),
    }
}

/// Fewest columns worth spending on a trailing intent. Below this the note is
/// an open paren and an ellipsis, which is noise beside the command rather
/// than context for it — so it is dropped entirely instead.
const INTENT_MIN_COLS: usize = 12;

/// Columns of separator and parens the intent note wraps itself in.
const INTENT_CHROME_COLS: usize = 5;

/// Lead-in of the trailing intent note. The renderer splits on this to dim the
/// note, so the two must stay the same string.
pub(super) const INTENT_LEAD: &str = "  (";

/// The trailing `  (checking the tag)` note, sized to whatever columns the
/// command left over, or `None` when there is no room / no intent.
///
/// Deliberately last and deliberately optional: the intent is model-generated
/// prose that can drift from what actually executes, so it never competes with
/// the command for the eye and is the first thing dropped when the row is
/// tight. The command is what runs; this only says what it was for.
pub(super) fn intent_note(subject: Option<&str>, used_cols: usize, width: u16) -> Option<String> {
    let subject = subject?;
    let room = usize::from(width)
        .saturating_sub(used_cols)
        .saturating_sub(INTENT_CHROME_COLS);
    if room < INTENT_MIN_COLS {
        return None;
    }
    Some(format!(
        "{INTENT_LEAD}{})",
        elide_middle_cols(subject, room)
    ))
}

pub(super) fn arg_hint(card: &StepCard, budget: usize) -> String {
    let Some(raw) = hint_field(card) else {
        return String::new();
    };
    if card.name.eq_ignore_ascii_case("bash") {
        elide_tail_at_word(strip_cd_prefix(raw), budget)
    } else {
        elide_middle(raw, budget)
    }
}

/// Width assumed when no terminal reports one (pipes, plain mode under a
/// non-tty). Matches the TUI's own fallback.
pub(super) const ASSUMED_WIDTH: u16 = 80;

/// Columns the tool badge and its ` · ` separator occupy in front of the
/// command, for a badge of typical length (`bash`, `read_file`).
const TOOL_BADGE_COLS: usize = 14;

/// Columns the `✔ ` severity glyph and the `approved ` / `denied ` verb take
/// on a transcript receipt, in front of the badge.
const APPROVAL_VERB_COLS: usize = 12;

/// Where an approval line is being drawn. The two frames pay for different
/// chrome — the transcript receipt carries a severity glyph and an `approved`
/// verb, the pinned modal header carries neither — so charging both the same
/// overhead made the modal throw away a dozen columns it actually had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ApprovalFrame {
    /// The `✔ approved bash · …` row in the transcript.
    Transcript,
    /// The `tool: bash · …` line in the pinned approval modal.
    Modal,
}

impl ApprovalFrame {
    /// Columns this frame spends on everything that is not the command.
    fn overhead(self) -> usize {
        match self {
            Self::Transcript => APPROVAL_VERB_COLS + TOOL_BADGE_COLS,
            Self::Modal => TOOL_BADGE_COLS,
        }
    }
}

/// Display columns `s` occupies in a terminal. Not `len()`: a CJK char is
/// three bytes and two columns, an emoji four and two, so byte-budgeting a
/// line pinned to one row is what makes it wrap.
pub(super) fn width_of(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// Truncate `s` to `budget` display COLUMNS by cutting out of the middle,
/// keeping head and tail. Column-accurate counterpart to [`elide_middle`],
/// which budgets in bytes and is kept for the byte-budgeted card hints.
pub(super) fn elide_middle_cols(s: &str, budget: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if width_of(s) <= budget {
        return s.to_string();
    }
    let keep = budget.saturating_sub(ELLIPSIS_OVERHEAD);
    let head_cols = keep / 2;
    let tail_cols = keep - head_cols;
    let mut head = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > head_cols {
            break;
        }
        head.push(c);
        used += w;
    }
    let mut tail = String::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > tail_cols {
            break;
        }
        tail.insert(0, c);
        used += w;
    }
    // Overlapping head and tail would repeat text; prefer the head alone.
    if head.len() + tail.len() > s.len() {
        return format!("{head}…");
    }
    format!("{head}…{tail}")
}

/// What a bash call with no `command` argument is: a shell, not a no-op. Named
/// so the note never renders as a bare tool category.
const NO_ARGS_PLACEHOLDER: &str = "(interactive)";

/// `tool · command`, one line, for a row that records an approval decision.
///
/// The argument IS the approval target: `bash` alone is a category, so a
/// prompt (or a receipt) that shows only the tool name cannot be reviewed and
/// becomes a rubber stamp. Deliberately different from [`arg_hint`] in one
/// way — this elides the MIDDLE even for bash, because the thing an operator
/// must not miss here is a destructive suffix (`--force`, `-rf`, the target
/// path), and head-keep truncation is exactly what hides it.
pub(super) fn approval_summary(
    tool: &str,
    input: &Value,
    width: u16,
    frame: ApprovalFrame,
) -> String {
    let budget = usize::from(width)
        .saturating_sub(frame.overhead())
        .max(ARG_HINT_MIN);
    let Some(raw) = hint_field_of(tool, input) else {
        return format!("{tool} · {NO_ARGS_PLACEHOLDER}");
    };
    let raw = if tool.eq_ignore_ascii_case("bash") {
        strip_cd_prefix(raw)
    } else {
        raw
    };
    // A heredoc or an `&&` chain is still one decision, so it stays one row:
    // newlines collapse to `; ` (valid shell punctuation, so the line reads as
    // the sequence it is) and the row says how many lines it stood for. The
    // unabridged text is in the modal body and on the card.
    let segments: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|l: &&str| !l.is_empty())
        .collect();
    // Count the segments the row actually stands for, not raw `lines()`:
    // blank lines are dropped above, so counting them promised the reader
    // more hidden text than `…` was standing in for.
    let extra = segments.len().saturating_sub(1);
    let one_line = if extra == 0 {
        // Single segment: collapse runs of inner whitespace so an indented
        // one-liner does not spend budget on its own padding.
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        segments.join("; ")
    };
    if one_line.is_empty() {
        return format!("{tool} · {NO_ARGS_PLACEHOLDER}");
    }
    let suffix = if extra > 0 {
        format!(" (+{extra} lines)")
    } else {
        String::new()
    };
    let room = budget.saturating_sub(width_of(&suffix)).max(ARG_HINT_MIN);
    let cmd = elide_middle_cols(&one_line, room);
    let line = format!("{tool} · {cmd}{suffix}");
    // The intent trails the command and only if the row has columns left. It
    // is model-written and can drift from what executes, so it is context at
    // the end, never the thing on the approval line.
    match intent_note(
        description_of(input).as_deref(),
        frame.overhead() + width_of(&cmd) + width_of(&suffix),
        width,
    ) {
        Some(note) => format!("{line}{note}"),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::agent::cli::step::StepCard;

    const TEST_WIDTH: u16 = 80;

    #[test]
    fn hint_budget_grows_with_the_terminal_and_has_a_floor() {
        // The bug: a flat 40 columns of command on every terminal, so a wide
        // one showed `… ` with two thirds of the row empty.
        assert!(hint_budget(200) > hint_budget(TEST_WIDTH));
        assert_eq!(hint_budget(40), ARG_HINT_MIN);
        assert_eq!(hint_budget(0), ARG_HINT_MIN);
    }

    #[test]
    fn a_truncated_command_never_ends_mid_token() {
        let cmd = "cargo clippy --all --all-targets --no-deps --locked -- -D warnings";
        for budget in [24usize, 30, 40, 55] {
            let hint = elide_tail_at_word(cmd, budget);
            let body = hint.trim_end_matches('…');
            assert!(
                cmd.starts_with(body),
                "hint must be a prefix of the command: {hint}"
            );
            assert!(hint.len() <= budget, "over budget {budget}: {hint}");
        }
    }

    #[test]
    fn strip_cd_prefix_removes_session_constant_cd() {
        assert_eq!(
            strip_cd_prefix(r#"cd /some/path && grep -n "x" f.rs"#),
            r#"grep -n "x" f.rs"#
        );
        assert_eq!(
            strip_cd_prefix(r#"grep -n "x" f.rs"#),
            r#"grep -n "x" f.rs"#
        );
    }

    #[test]
    fn hint_field_picks_bash_command_over_an_earlier_sorting_key() {
        let c = StepCard::new(
            "s1".into(),
            "bash".into(),
            serde_json::json!({ "description": "run tests", "command": "cargo test" }),
        );
        assert_eq!(hint_field(&c), Some("cargo test"));
    }

    #[test]
    fn elide_middle_keeps_head_and_tail_so_differing_tails_differ() {
        let a = "/Users/x/Projects/mur/mur-core/src/cmd/agent/cli/alpha.rs";
        let b = "/Users/x/Projects/mur/mur-core/src/cmd/agent/cli/beta.rs";
        let hint_a = elide_middle(a, 40);
        let hint_b = elide_middle(b, 40);
        assert_ne!(hint_a, hint_b, "tails must survive truncation");
    }

    #[test]
    fn elide_middle_on_long_multibyte_path_does_not_panic_and_respects_budget() {
        let long_cjk = "/專案/".repeat(40);
        let budget = 40;
        let hint = elide_middle(&long_cjk, budget);
        assert!(hint.len() <= budget + 4, "grossly over budget: {hint}");
    }

    /// The reported defect: the approval row said `bash` and nothing else, so
    /// there was nothing to review.
    #[test]
    fn approval_summary_names_the_command_not_just_the_tool() {
        let s = approval_summary(
            "bash",
            &serde_json::json!({ "command": "git push -u origin feat/channel-numbering" }),
            TEST_WIDTH,
            ApprovalFrame::Transcript,
        );
        assert_eq!(s, "bash · git push -u origin feat/channel-numbering");
    }

    #[test]
    fn approval_summary_keeps_the_dangerous_tail() {
        let s = approval_summary(
            "bash",
            &serde_json::json!({
                "command": "find /Users/david/Projects/mur/target/debug/build -type d -name 'out' -prune -exec rm -rf {} +"
            }),
            TEST_WIDTH,
            ApprovalFrame::Transcript,
        );
        assert!(s.contains("rm -rf"), "tail must survive elision: {s}");
        assert!(s.starts_with("bash · find"), "head must survive too: {s}");
    }

    #[test]
    fn approval_summary_is_one_line_for_a_heredoc() {
        let s = approval_summary(
            "bash",
            &serde_json::json!({ "command": "cat <<'EOF' > f.txt\nalpha\nbeta\nEOF" }),
            TEST_WIDTH,
            ApprovalFrame::Transcript,
        );
        assert!(!s.contains('\n'), "must stay one row: {s}");
        assert!(s.contains("(+3 lines)"), "must say what it stood for: {s}");
    }

    #[test]
    fn approval_summary_never_renders_a_bare_tool_name() {
        for input in [serde_json::json!({}), serde_json::Value::Null] {
            let s = approval_summary("bash", &input, TEST_WIDTH, ApprovalFrame::Modal);
            assert_eq!(s, "bash · (interactive)");
        }
    }

    /// At 40 columns the `ARG_HINT_MIN` floor is what binds, not the frame
    /// arithmetic; the line must still fit the terminal it is drawn in.
    #[test]
    fn approval_summary_fits_a_narrow_terminal() {
        let cmd = "cargo clippy --all --all-targets --no-deps --locked -- -D warnings";
        let width = 40u16;
        let s = approval_summary(
            "bash",
            &serde_json::json!({ "command": cmd }),
            width,
            ApprovalFrame::Transcript,
        );
        assert!(
            width_of(&s) <= usize::from(width),
            "over budget on a {width}-col terminal (width {}): {s}",
            width_of(&s)
        );
    }

    /// The modal header has no `approved ` prefix, so it must not be charged
    /// for one: a frame that spends fewer columns on chrome gets more of the
    /// command. Same width, same command, strictly more visible in the modal.
    #[test]
    fn each_frame_is_charged_only_for_its_own_chrome() {
        let cmd = "cargo clippy --all --all-targets --no-deps --locked -- -D warnings --fix";
        let args = serde_json::json!({ "command": cmd });
        let modal = approval_summary("bash", &args, 60, ApprovalFrame::Modal);
        let transcript = approval_summary("bash", &args, 60, ApprovalFrame::Transcript);
        assert!(
            width_of(&modal) > width_of(&transcript),
            "modal pays less chrome so it must show more: {modal} vs {transcript}"
        );
    }

    /// Budget is columns, not bytes. For CJK a byte budget UNDER-fills: three
    /// bytes buy two columns, so a byte-counted line stopped at about
    /// two-thirds of the row and threw away the rest of the command for
    /// nothing. It must still never exceed the row.
    #[test]
    fn a_cjk_command_is_budgeted_in_columns_not_bytes() {
        let cmd = format!("git commit -m \"{}\"", "更新中文說明文件".repeat(6));
        let width = 80u16;
        let args = serde_json::json!({ "command": cmd });
        let s = approval_summary("bash", &args, width, ApprovalFrame::Transcript);
        assert!(
            width_of(&s) <= usize::from(width),
            "wraps the row at {width} cols (width {}): {s}",
            width_of(&s)
        );
        // The byte-budgeted version produced a line this much narrower than
        // the space it had; columns must actually use the row.
        let budget = usize::from(width) - ApprovalFrame::Transcript.overhead();
        assert!(
            width_of(&s) >= budget,
            "under-fills the row (width {} of {budget} available): {s}",
            width_of(&s)
        );
    }

    /// `(+N lines)` counts the segments the row actually stands for. Blank
    /// lines are dropped from the joined text, so counting raw `lines()`
    /// promised segments that were not there.
    #[test]
    fn extra_line_count_matches_the_segments_that_survived() {
        let s = approval_summary(
            "bash",
            &serde_json::json!({ "command": "alpha\n\n\nbeta\n\ngamma" }),
            TEST_WIDTH,
            ApprovalFrame::Transcript,
        );
        assert!(s.contains("(+2 lines)"), "three segments, two extra: {s}");
    }

    /// The intent trails the command on an approval line, in parens, and the
    /// command comes first. Whatever the note says, the command is what runs.
    #[test]
    fn an_approval_line_trails_the_intent_after_the_command() {
        let s = approval_summary(
            "bash",
            &serde_json::json!({
                "command": "git push -u origin feat/x",
                "description": "publish the branch"
            }),
            120,
            ApprovalFrame::Transcript,
        );
        let cmd_at = s.find("git push").expect("command present");
        let note_at = s.find("publish").expect("intent present");
        assert!(cmd_at < note_at, "command must lead: {s}");
        assert!(s.ends_with("(publish the branch)"), "{s}");
    }

    /// The intent is the first thing sacrificed for columns. A long command on
    /// a narrow row keeps all of its own budget.
    #[test]
    fn a_narrow_approval_line_drops_the_intent_not_the_command() {
        let args = serde_json::json!({
            "command": "git push --force-with-lease origin feat/channel-numbering",
            "description": "publish the renumbered channel branch"
        });
        let wide = approval_summary("bash", &args, 200, ApprovalFrame::Transcript);
        let narrow = approval_summary("bash", &args, 48, ApprovalFrame::Transcript);
        assert!(wide.contains("publish the"), "wide row has room: {wide}");
        assert!(
            !narrow.contains("publish"),
            "narrow row must drop the note: {narrow}"
        );
        assert!(
            narrow.ends_with("numbering"),
            "the dangerous tail survives regardless: {narrow}"
        );
    }

    /// `split_intent` is what the renderer styles on, so it must agree with
    /// what `approval_summary` wrote — and must not mistake shell parens for a
    /// note.
    #[test]
    fn split_intent_round_trips_and_ignores_shell_parens() {
        let s = approval_summary(
            "bash",
            &serde_json::json!({
                "command": "ls",
                "description": "look around"
            }),
            120,
            ApprovalFrame::Transcript,
        );
        let (cmd, note) = split_intent(&s);
        assert_eq!(note, "  (look around)", "{s}");
        assert!(cmd.ends_with("ls"), "{cmd}");

        let shell = approval_summary(
            "bash",
            &serde_json::json!({ "command": "echo hi && (cd x && ls)" }),
            120,
            ApprovalFrame::Transcript,
        );
        let (cmd, note) = split_intent(&shell);
        assert_eq!(note, "", "a subshell is not an intent note: {shell}");
        assert!(cmd.contains("(cd x && ls)"), "{cmd}");
    }

    /// A wide multibyte intent is measured in columns, not bytes, so the note
    /// cannot push a pinned row into a wrap.
    #[test]
    fn a_cjk_intent_is_budgeted_in_columns() {
        let width = 72;
        let s = approval_summary(
            "bash",
            &serde_json::json!({
                "command": "git push -u origin feat/x",
                "description": "發佈這個分支到遠端倉庫以便審查"
            }),
            width,
            ApprovalFrame::Transcript,
        );
        assert!(
            width_of(&s) + APPROVAL_VERB_COLS <= usize::from(width),
            "row overflows its width (cols {} of {width}): {s}",
            width_of(&s) + APPROVAL_VERB_COLS
        );
    }
}
