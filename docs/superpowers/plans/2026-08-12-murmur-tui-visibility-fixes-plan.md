# murmur TUI visibility fixes — implementation plan

Spec: `docs/superpowers/specs/2026-08-12-murmur-tui-visibility-fixes-design.md`
Branch: `fix/murmur-tui-visibility`

> **Execute with `mur-executing-plans`.** Tasks 1–4 are a chain and must run in
> order. Tasks 5, 6, 7 are independent of the chain and of each other.

## Goal

Stop the murmur TUI from silently dropping content the operator must read: the
approval modal's key row, the settlement's text, and the reply hidden behind the
suggested-reply chooser.

## Architecture

`mur-agent-runtime` builds a per-turn settlement as fenced Markdown text and
appends it to the reply; `mur-core`'s TUI (`cmd/agent/cli/`) renders replies with
ratatui in an Inline viewport whose live band is drawn by `ui.rs::push_message`
every frame. This plan keeps that split: the runtime stops truncating text it
cannot measure, and the TUI — which knows the width via `App::width` — takes
over presentation. The approval modal and the chooser band are pure `ui.rs`
layout fixes.

## Tech stack

Rust edition 2024, ratatui, `unicode-width` (already a `mur-core` dependency at
`mur-core/Cargo.toml:126`), `cargo nextest` for tests.

## Global Constraints

Every task implicitly includes all of these.

- No hardcoded values — new tunables become named `const`s beside the existing
  ones in the same file.
- Single source file ≤ 800 lines; `ui.rs` is already 1636 lines, so new
  rendering code goes in new sibling modules, never appended to `ui.rs`.
- Run tests with `cargo nextest`, never plain `cargo test` — plain `cargo test`
  reports ~7 false failures in `mur-core`.
- `mur-core` tests need the environment prefix
  `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`.
  The first run compiles LanceDB/Arrow and takes several minutes; that is
  expected, not a hang.
- Brand name in user-visible strings is uppercase `MUR`.
- Every task ends with `cargo fmt` and a commit. CI runs
  `cargo clippy --workspace --all-targets -- -D warnings`.
- Do not touch `flush_finished` / `band_inner_rows` / `band_capacity`. Their
  chooser-blind capacity rule is deliberate (`ui.rs:463-471`) and out of scope.

## File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-agent-runtime/src/turn_ledger.rs` | modify | Stop truncating to widths it cannot know; emit a plain settlement marker |
| `mur-core/src/cmd/agent/cli/settlement.rs` | **new** | Split the settlement fence out of a reply; parse its rows; render the card at a given width |
| `mur-core/src/cmd/agent/cli/theme.rs` | modify | One new colour, `card_bg`, in each of the three skins |
| `mur-core/src/cmd/agent/cli/app.rs` | modify | `ChatMsg.settlement` field, populated where an agent turn is finalized |
| `mur-core/src/cmd/agent/cli/mod.rs` | modify | Declare `mod settlement;` |
| `mur-core/src/cmd/agent/cli/ui.rs` | modify | Paint the settlement card; pin the HITL key row; chooser floor; transcript scroll marker |

---

## Task 1 — Runtime stops truncating settlement text

The runtime cuts strings at 72 / 80 / 120 characters before any renderer sees
them. It cannot know the terminal width, so every one of those numbers is wrong
somewhere. Raise them all to a single backstop whose only job is to stop a
runaway error dump, and let the renderer decide what fits.

### Interfaces

**Consumes:** nothing.

**Produces:** the settlement text format that Task 2 parses —

```
\n\n```\n─ settlement ─\n<row>\n<row>\n…\n```
```

where each `<row>` is either `  <glyph> <text>` with `<glyph>` one of
`✔ ~ ✘ ⚠`, or a detail line indented by exactly six spaces. There is no
trailing rule line; the closing fence terminates the card. Row text may be of
any length and contains no newlines.

### Steps

- [x] **Write the failing test.** Append to the `mod tests` block in
  `mur-agent-runtime/src/turn_ledger.rs`, just before its closing brace:

  ```rust
      #[test]
      fn long_detail_survives_to_the_renderer() {
          // The runtime cannot know the terminal width, so it must not decide
          // what fits. Only a runaway-dump backstop remains.
          let mut l = TurnLedger::default();
          let long = "e".repeat(300);
          l.record(act("bash", "cargo test", Outcome::Failed(long.clone())));
          let card = render(&l);
          assert!(card.contains(&long), "detail was truncated: {card}");
          assert!(!card.contains('…'), "nothing should be elided here: {card}");
      }

      #[test]
      fn the_settlement_header_carries_no_fixed_width_rule() {
          let mut l = TurnLedger::default();
          l.record(act("edit_file", "src/lib.rs", Outcome::Ok));
          let card = render(&l);
          assert!(card.contains("─ settlement ─\n"), "{card}");
          // A hard-coded rule cannot match a pane it never measured.
          assert!(!card.contains("──────────"), "{card}");
      }
  ```

- [x] **Watch it fail.**

  ```
  cargo nextest run -p mur-agent-runtime turn_ledger
  ```

  Expect `FAIL … long_detail_survives_to_the_renderer` and
  `FAIL … the_settlement_header_carries_no_fixed_width_rule`.

- [x] **Add the backstop constant.** In `mur-agent-runtime/src/turn_ledger.rs`,
  immediately above `fn truncate`, insert:

  ```rust
  /// The only length limit the runtime still applies. Not a display width — it
  /// exists so one runaway error dump cannot flood the reply. The renderer owns
  /// what fits, because it is the only thing that knows the pane.
  const RUNAWAY_BACKSTOP: usize = 400;
  ```

- [x] **Raise every cap to the backstop.** Four edits in the same file, leaving
  `truncate`'s newline flattening untouched — a newline really would break a row.

  - In `describe_target`, replace `truncate(raw, 72)` with
    `truncate(raw, RUNAWAY_BACKSTOP)`.
  - In `clean_reason`, replace `truncate(s, 80)` with
    `truncate(s, RUNAWAY_BACKSTOP)`.
  - In `classify`, replace `truncate(detail, 120)` with
    `truncate(detail, RUNAWAY_BACKSTOP)`.
  - In `classify`, replace `truncate(content.trim(), 120)` with
    `truncate(content.trim(), RUNAWAY_BACKSTOP)`.

- [x] **Drop the fixed-width rules.** In `pub fn render`:

  - Replace the opening line

    ```rust
    let mut out = String::from("\n\n```\n─ settlement ─────────────────────────────\n");
    ```

    with

    ```rust
    let mut out = String::from("\n\n```\n─ settlement ─\n");
    ```

  - Replace the closing line

    ```rust
    out.push_str("──────────────────────────────────────────\n```");
    ```

    with

    ```rust
    out.push_str("```");
    ```

    Note the last row already ends in `\n`, so the fence still starts its own
    line.

- [x] **Update the test that pinned the old 72-char cap.** In
  `describe_target_picks_the_identifying_argument`, replace

  ```rust
          assert!(describe_target("bash", &long).chars().count() <= 72);
  ```

  with

  ```rust
          assert!(describe_target("bash", &long).chars().count() <= RUNAWAY_BACKSTOP);
  ```

  Leave the two lines below it alone — the `"a\nb"` → `"a b"` assertion is the
  newline flattening, which must survive.

- [x] **Watch it pass.**

  ```
  cargo nextest run -p mur-agent-runtime turn_ledger
  ```

  Expect `16 tests run: 16 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "fix(runtime): stop truncating settlement text the runtime cannot measure"
  ```

---

## Task 2 — Split the settlement out of the reply text

The card needs the pane width, which is only known at paint time. Keep it out of
the cached Markdown render by carrying it on the message as its own field.

### Interfaces

**Consumes:** Task 1's settlement text format.

**Produces:**

- `mur-core/src/cmd/agent/cli/settlement.rs`, declared as `mod settlement;` in
  `mur-core/src/cmd/agent/cli/mod.rs`:

  ```rust
  /// Split a settlement card off the end of an agent reply.
  /// Returns (reply_without_card, card_body). `card_body` lines exclude the
  /// fences and the `─ settlement ─` marker.
  pub fn split(text: &str) -> (String, Option<String>);
  ```

- `mur-core/src/cmd/agent/cli/app.rs`: `ChatMsg` gains
  `pub settlement: Option<String>`, populated on the two paths that finalize an
  agent turn.

### Steps

- [x] **Create the module with a failing test.** Write
  `mur-core/src/cmd/agent/cli/settlement.rs`:

  ```rust
  //! The per-turn settlement card: split off the reply, parsed, and rendered at
  //! the pane's real width.
  //!
  //! The runtime emits it as fenced text so every non-TUI consumer (`mur agent
  //! send`, `--plain`, logs, the Hub) can read it. The TUI upgrades that text to
  //! a card, which is why the split happens here and not in the Markdown
  //! renderer: the card needs a width, and `ChatMsg::rendered` is cached
  //! width-free.

  /// The marker the runtime writes as the fence's first line.
  const MARKER: &str = "─ settlement ─";

  /// Split a settlement card off the end of an agent reply.
  ///
  /// Returns the reply with the card removed, and the card's body — every line
  /// between the marker and the closing fence. Returns `(text, None)` unchanged
  /// when there is no card, which is the common case: most turns do not earn
  /// one.
  pub fn split(text: &str) -> (String, Option<String>) {
      let Some(fence_at) = text.rfind("```\n") else {
          return (text.to_string(), None);
      };
      let after_fence = &text[fence_at + 4..];
      let Some(rest) = after_fence.strip_prefix(MARKER) else {
          return (text.to_string(), None);
      };
      let rest = rest.strip_prefix('\n').unwrap_or(rest);
      let Some(close_at) = rest.find("```") else {
          return (text.to_string(), None);
      };
      let body = rest[..close_at].trim_end_matches('\n').to_string();
      let head = text[..fence_at].trim_end_matches('\n').to_string();
      (head, Some(body))
  }

  #[cfg(test)]
  mod tests {
      use super::split;

      #[test]
      fn splits_the_card_off_the_prose() {
          let reply = "did the thing\n\n```\n─ settlement ─\n  ✔ bash · cargo test\n```";
          let (head, card) = split(reply);
          assert_eq!(head, "did the thing");
          assert_eq!(card.as_deref(), Some("  ✔ bash · cargo test"));
      }

      #[test]
      fn an_ordinary_code_fence_is_not_a_settlement() {
          let reply = "look:\n\n```\nfn main() {}\n```";
          let (head, card) = split(reply);
          assert_eq!(head, reply);
          assert!(card.is_none());
      }

      #[test]
      fn a_reply_with_no_fence_is_returned_whole() {
          let (head, card) = split("just prose");
          assert_eq!(head, "just prose");
          assert!(card.is_none());
      }
  }
  ```

- [x] **Declare the module.** In `mur-core/src/cmd/agent/cli/mod.rs`, find the
  block of `mod` declarations near the top and add, in alphabetical position:

  ```rust
  mod settlement;
  ```

- [x] **Watch it fail to compile, then pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core settlement::tests
  ```

  Expect `3 tests run: 3 passed`. (The module is new, so there is no red phase
  for `split` itself; the red phase for the *behaviour* is the next step.)

- [x] **Write the failing test for the field.** In
  `mur-core/src/cmd/agent/cli/app.rs`, in the existing `#[cfg(test)] mod tests`
  block, append (it uses that module's own `fn app()` helper, the same way
  `finished_agent_turn_caches_markdown` does):

  ```rust
      #[test]
      fn finishing_a_turn_moves_the_settlement_off_the_body() {
          let mut a = app();
          a.begin_user_turn("hi");
          a.finish_agent_turn(
              "did it\n\n```\n─ settlement ─\n  ✔ bash · cargo test\n```".to_string(),
              None,
          );
          let m = a.messages.last().expect("a message");
          assert_eq!(m.text, "did it", "card must not stay in the body");
          assert_eq!(
              m.settlement.as_deref(),
              Some("  ✔ bash · cargo test"),
              "card must be carried separately so it can be drawn at the pane width"
          );
      }
  ```

- [x] **Watch it fail.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core finishing_a_turn_moves_the_settlement
  ```

  Expect a compile error: no field `settlement` on `ChatMsg`.

- [x] **Add the field.** In `mur-core/src/cmd/agent/cli/app.rs`, in
  `pub struct ChatMsg`, after the `step` field, add:

  ```rust
      /// The turn's settlement card, lifted out of `text` so it can be drawn at
      /// the pane's real width every frame. `rendered` is cached width-free, so
      /// a card baked into it would keep a stale width across a resize.
      pub settlement: Option<String>,
  ```

  Then add `settlement: None,` to each of the three `Self { … }` literals in
  `impl ChatMsg` — in `new`, in `agent_rendered`, and in `tool`.

- [x] **Populate it on the resume path.** In `impl ChatMsg`, replace the body of
  `agent_rendered`:

  ```rust
      /// A finished agent message whose markdown is pre-rendered (resume path).
      fn agent_rendered(text: String) -> Self {
          let (text, settlement) = super::settlement::split(&text);
          let rendered = Some(markdown::render(&text).lines);
          Self {
              role: Role::Agent,
              severity: Severity::Info,
              text,
              thinking: String::new(),
              streaming: false,
              rendered,
              step: None,
              settlement,
          }
      }
  ```

- [x] **Populate it on the live path.** In `pub fn finish_agent_turn`, replace

  ```rust
          if let Some(m) = self.streaming_agent_mut() {
              if !reply.is_empty() {
                  m.text = reply;
              }
              m.streaming = false;
              m.rendered = Some(markdown::render(&m.text).lines);
              body = Some(m.text.clone());
  ```

  with

  ```rust
          if let Some(m) = self.streaming_agent_mut() {
              if !reply.is_empty() {
                  m.text = reply;
              }
              let (text, settlement) = super::settlement::split(&m.text);
              m.text = text;
              m.settlement = settlement;
              m.streaming = false;
              m.rendered = Some(markdown::render(&m.text).lines);
              body = Some(m.text.clone());
  ```

- [x] **Watch it pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core settlement
  ```

  Expect `4 tests run: 4 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "feat(murmur): carry the settlement card beside the reply body"
  ```

---

## Task 3 — Render the settlement as a card

### Interfaces

**Consumes:** `settlement::split` from Task 2; `Theme` from `theme.rs`.

**Produces:**

- `theme.rs`: `pub card_bg: Color` on `Theme`, set in `DARK`, `LIGHT`, `MUR`.
- `settlement.rs`:

  ```rust
  pub fn card_lines(
      body: &str,
      theme: &'static super::theme::Theme,
      width: u16,
  ) -> Vec<ratatui::text::Line<'static>>;
  ```

  Every returned line is padded to exactly `width` display columns and carries
  `theme.card_bg`, so the card reads as one block. Text wraps; nothing is cut.

### Steps

- [x] **Add the theme colour.** In `mur-core/src/cmd/agent/cli/theme.rs`, in
  `pub struct Theme`, after the `separator` field, add:

  ```rust
      pub card_bg: Color,      // settlement card background (one step off the pane)
  ```

  Then add one line to each of the three skins, after their `separator:` line:

  - `DARK`: `card_bg: Color::Rgb(0x1e, 0x1e, 0x1e),`
  - `LIGHT`: `card_bg: Color::Rgb(0xf1, 0xf1, 0xf7),`
  - `MUR`: `card_bg: Color::Rgb(0x14, 0x14, 0x2c),`

  These are the only three `Theme` struct literals in the workspace, so nothing
  else needs updating.

- [x] **Write the failing test.** Append to the `mod tests` block in
  `mur-core/src/cmd/agent/cli/settlement.rs`:

  ```rust
      use super::card_lines;
      use super::super::theme::DARK;
      use unicode_width::UnicodeWidthStr;

      fn plain(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
          lines
              .iter()
              .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
              .collect()
      }

      #[test]
      fn every_row_is_padded_to_the_pane_width() {
          let out = card_lines("  ✔ bash · cargo test", &DARK, 40);
          assert!(!out.is_empty());
          for row in plain(&out) {
              assert_eq!(row.width(), 40, "ragged row: {row:?}");
          }
      }

      #[test]
      fn long_detail_wraps_instead_of_being_cut() {
          let body = format!("  ✘ parallel_jobs · {}", "e".repeat(200));
          let narrow = card_lines(&body, &DARK, 40);
          let wide = card_lines(&body, &DARK, 100);
          assert!(
              narrow.len() > wide.len(),
              "narrow={} wide={} — text must reflow, not truncate",
              narrow.len(),
              wide.len()
          );
          for row in plain(&narrow) {
              assert!(!row.contains('…'), "nothing may be elided: {row:?}");
          }
      }

      #[test]
      fn the_card_names_itself() {
          let out = plain(&card_lines("  ✔ bash", &DARK, 40));
          assert!(out[0].contains("SETTLEMENT"), "{out:?}");
      }
  ```

- [x] **Watch it fail.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core settlement::tests
  ```

  Expect a compile error: no function `card_lines`.

- [x] **Implement the renderer.** Append to
  `mur-core/src/cmd/agent/cli/settlement.rs`, above its `mod tests`:

  ```rust
  use ratatui::style::{Modifier, Style};
  use ratatui::text::{Line, Span};
  use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

  /// Columns the glyph column occupies: two spaces, the glyph, one space.
  const GLYPH_COL: usize = 4;

  /// Narrower than this and the hanging indent costs more than it buys, so the
  /// card falls back to flush-left rows.
  const MIN_INDENT_WIDTH: u16 = 24;

  /// Colour for a row, chosen by its lead glyph.
  fn row_style(glyph: char, theme: &'static super::theme::Theme) -> Style {
      let fg = match glyph {
          '✔' => theme.success,
          '✘' => theme.error,
          '⚠' => theme.warn,
          _ => theme.agent_text,
      };
      Style::default().fg(fg).bg(theme.card_bg)
  }

  /// Break `s` into chunks no wider than `width` display columns.
  ///
  /// Greedy on word boundaries, falling back to a hard break for a single token
  /// longer than the line — a 200-character error string with no spaces still
  /// has to land somewhere, and cutting it is the one thing this card must not
  /// do.
  fn wrap(s: &str, width: usize) -> Vec<String> {
      if width == 0 {
          return vec![s.to_string()];
      }
      let mut out: Vec<String> = Vec::new();
      let mut line = String::new();
      let mut line_w = 0usize;
      for word in s.split(' ') {
          let word_w = word.width();
          if !line.is_empty() && line_w + 1 + word_w > width {
              out.push(std::mem::take(&mut line));
              line_w = 0;
          }
          if word_w > width {
              // Hard-break an unbreakable token.
              for c in word.chars() {
                  let cw = c.width().unwrap_or(0);
                  if line_w + cw > width {
                      out.push(std::mem::take(&mut line));
                      line_w = 0;
                  }
                  line.push(c);
                  line_w += cw;
              }
              continue;
          }
          if !line.is_empty() {
              line.push(' ');
              line_w += 1;
          }
          line.push_str(word);
          line_w += word_w;
      }
      if !line.is_empty() || out.is_empty() {
          out.push(line);
      }
      out
  }

  /// Pad `s` to exactly `width` display columns.
  fn pad(s: &str, width: usize) -> String {
      let w = s.width();
      if w >= width {
          return s.to_string();
      }
      format!("{s}{}", " ".repeat(width - w))
  }

  /// Draw the settlement card for `body` at `width` columns.
  ///
  /// Every row is padded to the full width and carries `theme.card_bg`, so the
  /// block reads as one surface rather than ragged text. Nothing is elided: the
  /// runtime already stopped guessing what fits, and this is the layer that
  /// actually knows.
  pub fn card_lines(
      body: &str,
      theme: &'static super::theme::Theme,
      width: u16,
  ) -> Vec<Line<'static>> {
      let w = width.max(1) as usize;
      let indent = if width >= MIN_INDENT_WIDTH { GLYPH_COL } else { 0 };
      let mut out = vec![
          Line::from(Span::styled(
              pad(" SETTLEMENT", w),
              Style::default()
                  .fg(theme.border_title)
                  .bg(theme.card_bg)
                  .add_modifier(Modifier::BOLD),
          )),
      ];
      for raw in body.lines() {
          let trimmed = raw.trim_start();
          let glyph = trimmed.chars().next().unwrap_or(' ');
          let style = row_style(glyph, theme);
          let is_row = matches!(glyph, '✔' | '✘' | '⚠' | '~');
          let (head, text) = if is_row {
              let rest = trimmed.chars().skip(1).collect::<String>();
              (format!("  {glyph} "), rest.trim_start().to_string())
          } else {
              (" ".repeat(indent.max(2)), trimmed.to_string())
          };
          let head_w = head.width();
          let avail = w.saturating_sub(head_w).max(1);
          for (i, chunk) in wrap(&text, avail).into_iter().enumerate() {
              let prefix = if i == 0 {
                  head.clone()
              } else {
                  " ".repeat(head_w)
              };
              out.push(Line::from(Span::styled(
                  pad(&format!("{prefix}{chunk}"), w),
                  style,
              )));
          }
      }
      out
  }
  ```

- [x] **Watch it pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core settlement
  ```

  Expect `7 tests run: 7 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "feat(murmur): draw the settlement as a width-aware card"
  ```

---

## Task 4 — Paint the card in the transcript

### Interfaces

**Consumes:** `settlement::card_lines` (Task 3), `ChatMsg.settlement` (Task 2).

**Produces:** nothing further; this closes the settlement chain.

### Steps

- [x] **Write the failing test.** Append a new module at the end of
  `mur-core/src/cmd/agent/cli/ui.rs`:

  ```rust
  #[cfg(test)]
  mod settlement_paint_tests {
      use super::push_message;
      use super::super::app::{ChatMsg, Role};
      use super::super::theme::DARK;

      #[test]
      fn a_carried_settlement_is_painted_after_the_body() {
          let mut m = ChatMsg::for_test(Role::Agent, "did it");
          m.settlement = Some("  ✔ bash · cargo test".into());
          let mut lines = Vec::new();
          push_message(&mut lines, &m, 0, &DARK, false, 60);
          let text: Vec<String> = lines
              .iter()
              .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
              .collect();
          assert!(
              text.iter().any(|l| l.contains("SETTLEMENT")),
              "card missing: {text:?}"
          );
          assert!(
              text.iter().any(|l| l.contains("cargo test")),
              "card body missing: {text:?}"
          );
      }

      #[test]
      fn a_message_without_one_paints_nothing_extra() {
          let m = ChatMsg::for_test(Role::Agent, "did it");
          let mut lines = Vec::new();
          push_message(&mut lines, &m, 0, &DARK, false, 60);
          let text: Vec<String> = lines
              .iter()
              .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
              .collect();
          assert!(!text.iter().any(|l| l.contains("SETTLEMENT")), "{text:?}");
      }
  }
  ```

- [x] **Watch it fail.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core settlement_paint_tests
  ```

  Expect `FAIL … a_carried_settlement_is_painted_after_the_body`.

- [x] **Paint it.** In `mur-core/src/cmd/agent/cli/ui.rs`, in `fn push_message`,
  in the `Role::Agent` arm, replace

  ```rust
          Role::Agent => {
              // Animated bullet while streaming, solid bullet once done — so an
              // in-progress turn reads as "live" at a glance vs. a finished one.
              push_agent_header(lines, m, spinner, theme);
              lines.extend(agent_body_lines(
                  &m.text,
                  m.streaming,
                  spinner,
                  theme,
                  m.rendered.as_ref(),
              ));
          }
  ```

  with

  ```rust
          Role::Agent => {
              // Animated bullet while streaming, solid bullet once done — so an
              // in-progress turn reads as "live" at a glance vs. a finished one.
              push_agent_header(lines, m, spinner, theme);
              lines.extend(agent_body_lines(
                  &m.text,
                  m.streaming,
                  spinner,
                  theme,
                  m.rendered.as_ref(),
              ));
              // Drawn here, not baked into `rendered`: this is the only place
              // that knows the pane width, and it runs every frame, so the card
              // reflows on resize for free.
              if let Some(body) = &m.settlement {
                  let inner = width.saturating_sub(u16::from(theme.inner_padding) * 2);
                  lines.extend(super::settlement::card_lines(body, theme, inner));
              }
          }
  ```

- [x] **Watch it pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core settlement
  ```

  Expect `9 tests run: 9 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "feat(murmur): paint the settlement card in the transcript"
  ```

---

## Task 5 — Pin the approval modal's key row

Independent of Tasks 1–4.

### Interfaces

**Consumes:** nothing. **Produces:** nothing consumed elsewhere.

### Steps

- [x] **Write the failing test.** Append a new module at the end of
  `mur-core/src/cmd/agent/cli/ui.rs`:

  ```rust
  #[cfg(test)]
  mod hitl_modal_tests {
      use super::render_hitl;
      use super::super::stream::HitlRequest;
      use ratatui::Terminal;
      use ratatui::backend::TestBackend;

      /// A tool input big enough that, wrapped, it fills the modal on its own.
      fn fat_request() -> HitlRequest {
          HitlRequest {
              hitl_id: "h1".into(),
              step_id: None,
              tool_name: "bash".into(),
              tool_input: serde_json::json!({
                  "command": "x".repeat(400),
                  "cwd": "/Volumes/Firecuda4tb/Projects/mur",
              }),
              prompt: "Run `bash`?".into(),
              created_at: std::time::Instant::now(),
          }
      }

      #[test]
      fn the_key_row_survives_an_oversized_input() {
          let mut term = Terminal::new(TestBackend::new(88, 24)).unwrap();
          term.draw(|f| render_hitl(f, &fat_request(), None)).unwrap();
          let dump = term.backend().to_string();
          assert!(
              dump.contains("approve"),
              "the operator cannot answer a gate whose keys are off-screen:\n{dump}"
          );
          assert!(dump.contains("deny"), "{dump}");
      }
  }
  ```

- [x] **Watch it fail.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core hitl_modal_tests
  ```

  Expect `FAIL … the_key_row_survives_an_oversized_input`.

- [x] **Split the modal.** In `mur-core/src/cmd/agent/cli/ui.rs`, in
  `fn render_hitl`:

  1. Change the `lines` builder so the key row is no longer pushed into it.
     Replace the two `lines.push(Line::from(vec![…]))` calls in the
     `if let Some(c) = grant_confirm { … } else { … }` block with assignments to
     a new `keys` binding:

     ```rust
     let keys = if let Some(c) = grant_confirm {
         let what = if c == 'a' {
             format!("`{}` for this session", hitl.tool_name)
         } else {
             "ALL tools for this session".to_string()
         };
         Line::from(vec![
             Span::styled(
                 format!("press [{c}] again"),
                 Style::default()
                     .fg(Color::Magenta)
                     .add_modifier(Modifier::BOLD),
             ),
             Span::raw(format!(" to allow {what} — any other key cancels")),
         ])
     } else {
         Line::from(vec![
             Span::styled(
                 "[y]",
                 Style::default()
                     .fg(Color::Green)
                     .add_modifier(Modifier::BOLD),
             ),
             Span::raw(" approve    "),
             Span::styled(
                 "[a]",
                 Style::default()
                     .fg(Color::Yellow)
                     .add_modifier(Modifier::BOLD),
             ),
             Span::raw(" always allow this tool (session)    "),
             Span::styled(
                 "[A]",
                 Style::default()
                     .fg(Color::Magenta)
                     .add_modifier(Modifier::BOLD),
             ),
             Span::raw(" allow all tools (session)    "),
             Span::styled(
                 "[n]",
                 Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
             ),
             Span::raw(" deny / Esc"),
         ])
     };
     ```

     Delete the now-unneeded `lines.push(Line::default());` that preceded that
     block — the layout gap replaces it.

  2. Replace the whole render tail — from `let block = Block::default()` to the
     closing `);` of the second `f.render_widget` — with:

     ```rust
     let block = Block::default()
         .borders(Borders::ALL)
         .border_style(Style::default().fg(Color::Yellow))
         .title(" approve tool call ");
     let inner = block.inner(area);
     f.render_widget(Clear, area);
     f.render_widget(block, area);

     // The key row is the only part of this modal that is never optional, so it
     // gets its own chunk. Previously it was the last entry in one clipped
     // Paragraph: a wrapped JSON input pushed it out of the box and left the
     // operator staring at a blocking gate with no visible way to answer it.
     let keys_h = Paragraph::new(keys.clone())
         .wrap(Wrap { trim: false })
         .line_count(inner.width.max(1)) as u16;
     let chunks = Layout::default()
         .direction(Direction::Vertical)
         .constraints([Constraint::Min(0), Constraint::Length(keys_h.max(1))])
         .split(inner);

     // The body truncates rather than wraps, so one input line costs one row and
     // the height is predictable; when it still overflows, say so on screen.
     let body_h = chunks[0].height as usize;
     if lines.len() > body_h && body_h > 0 {
         lines.truncate(body_h.saturating_sub(1));
         lines.push(Line::styled(
             format!("… {} more lines", body_h.saturating_sub(1)),
             Style::default().fg(Color::DarkGray),
         ));
     }
     f.render_widget(Paragraph::new(Text::from(lines)), chunks[0]);
     f.render_widget(
         Paragraph::new(keys).wrap(Wrap { trim: false }),
         chunks[1],
     );
     ```

  3. Make `lines` mutable-and-truncatable: it is already `let mut lines`, so no
     change is needed there.

  4. Truncate the JSON rows instead of letting them wrap. Replace

     ```rust
     for l in input.lines().take(12) {
         lines.push(Line::styled(
             l.to_string(),
             Style::default().fg(Color::DarkGray),
         ));
     }
     ```

     with

     ```rust
     let row_w = area.width.saturating_sub(2) as usize;
     for l in input.lines().take(12) {
         let row: String = if l.chars().count() > row_w && row_w > 1 {
             l.chars().take(row_w - 1).chain(['…']).collect()
         } else {
             l.to_string()
         };
         lines.push(Line::styled(row, Style::default().fg(Color::DarkGray)));
     }
     ```

  5. Replace the magic percentages at the top of the function. Change

     ```rust
     let area = centered_rect(70, 50, f.area());
     ```

     to

     ```rust
     let area = centered_rect(HITL_PCT_X, HITL_PCT_Y, f.area());
     ```

     and add, next to the other `const`s near the top of `ui.rs`:

     ```rust
     /// Approval modal size, as a percentage of the viewport.
     const HITL_PCT_X: u16 = 70;
     const HITL_PCT_Y: u16 = 50;
     ```

- [x] **Watch it pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core hitl_modal_tests
  ```

  Expect `1 test run: 1 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "fix(murmur): pin the approval modal's key row so an oversized input cannot hide it"
  ```

---

## Task 6 — Give the transcript a real floor under the chooser

Independent of every other task. Do not touch `flush_finished`.

### Interfaces

**Consumes:** nothing. **Produces:** nothing consumed elsewhere.

### Steps

- [x] **Write the failing test.** Append a new module at the end of
  `mur-core/src/cmd/agent/cli/ui.rs`:

  ```rust
  #[cfg(test)]
  mod chooser_floor_tests {
      use super::chooser_band_height;
      use super::super::app::App;
      use super::super::complete::{Candidate, CompletionState};

      fn option(display: &str, desc: &str) -> Candidate {
          Candidate {
              display: display.into(),
              insert: display.into(),
              desc: desc.into(),
              has_children: false,
          }
      }

      /// Three suggested replies, each with a description — the shape from the
      /// report.
      fn app_with_three_options() -> App {
          let mut a = App::test_fixture();
          a.completion = Some(CompletionState {
              items: vec![
                  option("open the PR", "wait for CI, then tag"),
                  option("stronger model", "the 4B one fakes tool calls"),
                  option("leave it", "change nothing"),
              ],
              selected: 0,
              spaced: true,
          });
          a
      }

      #[test]
      fn the_chooser_leaves_the_transcript_more_than_three_rows() {
          // Inline viewport is 20 rows; composer 3 + status 1 leaves 16.
          let h = chooser_band_height(&app_with_three_options(), 20, 3);
          assert!(
              h <= 8,
              "chooser took {h} rows, leaving the reply a peephole"
          );
      }

      #[test]
      fn a_short_terminal_still_gets_a_usable_chooser() {
          // The floor must yield rather than squeeze the chooser out: it is the
          // thing the operator has to act on.
          let h = chooser_band_height(&app_with_three_options(), 12, 3);
          assert!(h >= 5, "chooser unusable at {h} rows");
      }

      #[test]
      fn ctrl_up_still_reaches_the_spaced_form() {
          let mut a = app_with_three_options();
          a.chooser_grow = 6;
          assert_eq!(chooser_band_height(&a, 20, 3), 11);
      }
  }
  ```

  `App::test_fixture()` already exists under `#[cfg(test)]` in `app.rs`;
  `Candidate` and `CompletionState` are both `pub` in `complete.rs` with all
  fields public, so no new test constructors are needed.

- [x] **Watch it fail.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core chooser_floor_tests
  ```

  Expect `FAIL … the_chooser_leaves_the_transcript_more_than_three_rows`
  (it returns 11).

- [x] **Add the floor.** In `mur-core/src/cmd/agent/cli/ui.rs`, next to
  `const MIN_TRANSCRIPT_ROWS: u16 = 3;` add:

  ```rust
  /// Share of the viewport the transcript keeps when the chooser is open,
  /// in percent.
  ///
  /// `MIN_TRANSCRIPT_ROWS` alone is a floor for "the chooser must fit at all",
  /// not for "the reply is still readable" — at three rows, PageUp pages a
  /// twenty-line answer three lines at a time through a three-line peephole.
  /// This is the readable floor; the hard minimum still wins on a short
  /// terminal.
  const TRANSCRIPT_FLOOR_PCT: u16 = 40;
  ```

- [x] **Apply it.** Replace the body of `fn chooser_band_height` from the
  `let available = …` line down to (but not including) the final `clamp`
  expression:

  ```rust
      let chrome = input_height + 1; // composer + status line
      let full: u16 = state
          .items
          .iter()
          .map(|c| 2 + u16::from(!c.desc.is_empty())) // label + spacer (+ desc)
          .sum::<u16>()
          .saturating_add(2); // borders
      let compact = (state.items.len() as u16).saturating_add(2);
      // Prefer the readable floor; fall back to the hard minimum only when the
      // floor would squeeze the chooser below its compact form. The chooser is
      // what the operator must act on, so it never loses this trade.
      let roomy = total_h
          .saturating_sub(chrome)
          .saturating_sub((total_h * TRANSCRIPT_FLOOR_PCT / 100).max(MIN_TRANSCRIPT_ROWS));
      let tight = total_h.saturating_sub(chrome + MIN_TRANSCRIPT_ROWS);
      let available = if roomy >= compact { roomy } else { tight };
      // Take `compact` exactly when the spaced form does not fit — padding the
      // band out to `available` spends rows on nothing.
      let auto = if full <= available {
          full
      } else {
          compact.min(available).max(3)
      };
  ```

  Leave the trailing `chooser_grow` clamp line exactly as it is.

- [x] **Watch it pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core chooser_floor_tests
  ```

  Expect `3 tests run: 3 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "fix(murmur): give the transcript a readable floor under the suggested-reply chooser"
  ```

---

## Task 7 — Say when the transcript is hiding content

Independent of every other task.

### Interfaces

**Consumes:** nothing. **Produces:** nothing consumed elsewhere.

### Steps

- [x] **Write the failing test.** Append a new module at the end of
  `mur-core/src/cmd/agent/cli/ui.rs`:

  ```rust
  #[cfg(test)]
  mod scroll_marker_tests {
      use super::scroll_marker;

      #[test]
      fn silence_when_everything_fits() {
          assert_eq!(scroll_marker(0, 0), None);
      }

      #[test]
      fn following_the_tail_points_up() {
          assert_eq!(scroll_marker(25, 0).as_deref(), Some(" ↑ 25 more · PgUp "));
      }

      #[test]
      fn scrolled_back_points_the_way_home() {
          assert_eq!(
              scroll_marker(25, 10).as_deref(),
              Some(" ↑ 15 · PgDn to follow ")
          );
      }
  }
  ```

- [x] **Watch it fail.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core scroll_marker_tests
  ```

  Expect a compile error: no function `scroll_marker`.

- [x] **Add the marker.** In `mur-core/src/cmd/agent/cli/ui.rs`, immediately
  above `fn render_transcript`, insert:

  ```rust
  /// Right-hand title for the transcript's top border, or `None` when nothing
  /// is hidden.
  ///
  /// A band that silently drops the rows above it is indistinguishable from one
  /// that never had them — which is exactly how a reply behind the suggested-
  /// reply chooser reads as lost. `max_scroll` is the number of rows above the
  /// band; `scroll_back` is how many the operator has already walked up.
  fn scroll_marker(max_scroll: u16, scroll_back: u16) -> Option<String> {
      if max_scroll == 0 {
          return None;
      }
      Some(if scroll_back == 0 {
          format!(" ↑ {max_scroll} more · PgUp ")
      } else {
          format!(" ↑ {} · PgDn to follow ", max_scroll - scroll_back)
      })
  }
  ```

- [x] **Render it.** In `fn render_transcript`, replace the final line

  ```rust
      f.render_widget(output.block(block).scroll((offset, 0)), area);
  ```

  with

  ```rust
      let block = match scroll_marker(max_scroll, app.scroll_back) {
          Some(marker) => block.title(Line::from(marker).right_aligned()),
          None => block,
      };
      f.render_widget(output.block(block).scroll((offset, 0)), area);
  ```

  `Line::right_aligned()` and `Block::title(impl Into<Title>)` are both present
  in ratatui 0.29 (`mur-core/Cargo.toml:120`), and `Line` is already imported at
  the top of `ui.rs`. The `block` binding earlier in the function is immutable
  and is only moved at the end, so shadowing it here is fine.

- [x] **Watch it pass.**

  ```
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core scroll_marker_tests
  ```

  Expect `3 tests run: 3 passed`.

- [x] **Commit.** (Not performed: repository policy requires explicit permission.)

  ```
  cargo fmt && git add -A && git commit -m "fix(murmur): say when the transcript is hiding rows above the band"
  ```

---

## Final gate

- [x] **Full workspace check.**

  ```
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --check
  ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core
  cargo nextest run -p mur-agent-runtime
  ```

  All four must be clean. Paste the summary lines into the PR body — a claim
  without the output is not evidence.

- [ ] **Manual check, with a rebuilt runtime.** (Not run: this installs and
  restarts the local runtime, which requires explicit deployment permission.)
  `turn_ledger.rs` lives in
  `mur-agent-runtime`, and an agent loads its binary once at start, so the
  settlement fix does not reach a running agent until it is restarted:

  ```
  ./build.sh --release --install
  mur agent restart --stale
  murmur mur
  ```

  Then confirm, in one session: a tool approval shows its key row; a turn that
  edits or fails a tool renders the settlement as a shaded block with nothing
  cut; and when the suggested-reply chooser opens, at least nine rows of the
  reply remain visible with `↑ N more · PgUp` on the top border when there is
  more above.

  `mur agent restart --stale` does not see agents without a `running.lock`, and
  reports `25/25 ✓` while leaving some on the old binary. Verify with
  `mur agent status` that the agent you test actually restarted.
