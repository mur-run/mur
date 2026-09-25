//! `/browser` command: browser skill hub. `--add` attaches the built-in
//! `browser` skill to this agent; a mode argument (once attached) is sent to
//! the model as a turn.
//!
//! D5 (docs/superpowers/specs/2026-09-25-browser-skill-hub-design.md): this
//! variant is typed, never `SlashCmd::Unknown`, so `turn.rs`'s `submit()`
//! never routes it through `complete::matched_skill`'s fallthrough — that
//! guard is gated on `Unknown` and this dispatch arm has to do the whole job
//! itself, including the turn-start `matched_skill` would otherwise have done.

use super::*;

const SKILL_NAME: &str = "browser";

pub(super) async fn handle(app: &mut App, args: Vec<String>, tx: &mpsc::Sender<StreamMsg>) {
    if args.first().map(String::as_str) == Some("--add") {
        // D6: reuse `/skill add`'s own path unmodified. The global copy of
        // `mur-browser/SKILL.md` (shipped by `ensure_mur_skill`, D2/D3) is
        // the source `cmd_skill_add` parses; its `BUNDLE_ASSET_DIRS` copy
        // step then picks up `references/*.md` from the same directory —
        // no bespoke copy logic here.
        let source = app
            .home
            .join("skills")
            .join(SKILL_NAME)
            .join("SKILL.md")
            .to_string_lossy()
            .into_owned();
        run_manage(app, move |agent| manage::skill_add(&agent, &source)).await;
        return;
    }

    if !app.skills.iter().any(|c| c.display == "/browser") {
        app.push_system("browser skill not attached — run /browser --add first");
        return;
    }

    let instruction = if args.is_empty() {
        "Use the browser skill.".to_string()
    } else {
        format!("Use the browser skill. {}", args.join(" "))
    };
    app.clear_input();
    start_turn(app, instruction, tx);
}
