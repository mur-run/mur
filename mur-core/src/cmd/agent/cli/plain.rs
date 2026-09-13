//! The --plain non-TUI path, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

/// Pipe-safe fallback: read a line from stdin, stream the reply as plain text to
/// stdout, repeat. No ANSI, no TUI. Threads conversation context across turns.
pub(super) fn run_plain(
    home: &Path,
    agent: &str,
    auto: bool,
    auto_reads: bool,
    interactive: bool,
) -> Result<()> {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::io::Write as _;
    let out2 = RefCell::new(io::stdout());
    let mut context: Option<String> = None;
    let cwd = std::env::current_dir().ok();
    let (pricing, _book) = load_pricing(home, agent);
    // Tools the operator granted with `[a]` this session. Plain mode had no
    // such set, so `[a]lways` approved exactly one call — the prompt said one
    // thing and the code did another.
    let session_allow: RefCell<std::collections::HashSet<String>> =
        RefCell::new(Default::default());

    loop {
        if interactive {
            let _ = write!(out2.borrow_mut(), "you › ");
            let _ = out2.borrow_mut().flush();
        }
        let mut line = String::new();
        // Lock only per-read so a HITL callback (Task 3) can read stdin mid-turn.
        if io::stdin().lock().read_line(&mut line)? == 0 {
            break; // EOF (Ctrl-D / end of pipe)
        }
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let task_id = uuid::Uuid::now_v7().to_string();
        let params = build_params(&text, &task_id, context.as_deref(), None, cwd.as_deref());
        let streamed = Cell::new(false);
        // Track step_id → name from Started events so Completed can print the name.
        let step_names: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());

        let result = crate::a2a_dial::dial_message_streaming(
            home,
            agent,
            params,
            |delta, thinking, _task_id| {
                if !thinking {
                    streamed.set(true);
                    let _ = write!(out2.borrow_mut(), "{delta}");
                    let _ = out2.borrow_mut().flush();
                }
            },
            |hitl| {
                let id = hitl
                    .get("hitl_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let tool = hitl
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                // Second element is the audit attribution: only the interactive
                // branch has a human at the keyboard, so every other branch says
                // "auto" rather than claiming someone answered.
                let (allow, surface) = if auto {
                    eprintln!(
                        "[non-interactive: auto-approving tool-approval request (default; --ask to deny)]"
                    );
                    (true, "auto")
                } else if auto_reads && bash_class::is_readonly_call(tool, hitl.get("tool_input")) {
                    // Same lane as the TUI, same classifier. This mode used to
                    // ignore `--auto-reads` outright, so the identical flag
                    // behaved differently depending on how the CLI was started
                    // — and plain mode is exactly where unattended runs live.
                    eprintln!("  [auto-approved read-only {tool} (--auto-reads)]");
                    (true, "auto")
                } else if session_allow.borrow().contains(tool) {
                    eprintln!("  [auto-approved {tool} (session allow)]");
                    (true, "auto")
                } else if interactive {
                    // Outer loop releases stdin lock between reads (Task 2), so
                    // we can safely acquire a fresh lock here to prompt the user.
                    let mut o = io::stdout();
                    let _ = write!(o, "  tool approval: {tool} — [y]es / [a]lways / [n]o? ");
                    let _ = o.flush();
                    let mut ans = String::new();
                    let _ = io::stdin().lock().read_line(&mut ans);
                    let allowed = match ans.trim().chars().next() {
                        // [a] now means what it says. It used to approve just
                        // this one call while the prompt promised "always".
                        Some('a' | 'A') => {
                            session_allow.borrow_mut().insert(tool.to_string());
                            true
                        }
                        Some('y' | 'Y') => true,
                        _ => false,
                    };
                    (allowed, "cli")
                } else {
                    eprintln!(
                        "[non-interactive: auto-denying tool-approval request (--ask; drop it to allow)]"
                    );
                    (false, "auto")
                };
                let _ = dial_method(
                    home,
                    agent,
                    "tool/hitl_respond",
                    stream::hitl_respond_params(&id, allow, surface),
                    DialMode::RequireRunning,
                );
            },
            |step| {
                use crate::a2a_dial::StepEvent;
                match step {
                    StepEvent::Started {
                        step_id,
                        name,
                        args,
                        ..
                    } => {
                        // Derive a short arg hint: prefer "command" arg, else JSON.
                        let hint_raw = args
                            .get("command")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| args.to_string());
                        let hint: String = hint_raw.chars().take(PLAIN_STEP_HINT_MAX).collect();
                        step_names
                            .borrow_mut()
                            .insert(step_id.clone(), name.clone());
                        let _ = writeln!(out2.borrow_mut(), "→ {name} {hint}");
                        let _ = out2.borrow_mut().flush();
                    }
                    StepEvent::Completed {
                        step_id,
                        ok,
                        duration_ms,
                        ..
                    } => {
                        let glyph = if ok { '✔' } else { '✗' };
                        let name = step_names
                            .borrow()
                            .get(&step_id)
                            .cloned()
                            .unwrap_or_default();
                        let _ = writeln!(out2.borrow_mut(), "{glyph} {name} · {duration_ms}ms");
                        let _ = out2.borrow_mut().flush();
                    }
                }
            },
        );
        match result {
            Ok(task) => match stream::task_outcome(&task) {
                Ok((reply, tid)) => {
                    // Fall back to the final reply if the agent didn't stream deltas.
                    if !streamed.get() && !reply.trim().is_empty() {
                        write!(out2.borrow_mut(), "{reply}")?;
                    }
                    writeln!(out2.borrow_mut())?;
                    // Usage footer: total tokens + cost (reuse footer helpers).
                    if let Some(usage) = task.get("usage") {
                        let u = footer::parse_usage(usage);
                        match footer::turn_cost(&pricing, &u) {
                            Some(c) => {
                                let _ = writeln!(
                                    out2.borrow_mut(),
                                    "  {} tok · ${c:.3}",
                                    u.input + u.output
                                );
                            }
                            None => {
                                let _ = writeln!(out2.borrow_mut(), "  {} tok", u.input + u.output);
                            }
                        }
                    }
                    out2.borrow_mut().flush()?;
                    context = tid;
                }
                Err(cause) => {
                    writeln!(out2.borrow_mut(), "\nerror: {cause}")?;
                }
            },
            Err(e) => {
                writeln!(out2.borrow_mut(), "\nerror: {e:#}")?;
            }
        }
    }
    Ok(())
}
