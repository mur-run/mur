use super::*;

pub(super) fn run_open(
    action: Option<OpenAction>,
    json: bool,
    all: bool,
    check: bool,
) -> Result<()> {
    let home = crate::paths::mur_root(None);
    let cfg_path = home.join("config.yaml");
    match action {
        Some(OpenAction::Add { title, agent, next }) => {
            let id = crate::open_items::reported::report(&home, &agent, &title, next.as_deref())?;
            println!("Recorded (reported by {agent}): {id}");
        }
        Some(OpenAction::Done { id }) => {
            use crate::open_items::reported::Resolution;
            match crate::open_items::reported::resolve(&home, &id)? {
                Resolution::Closed { id, title } => println!("Resolved {id} — {title}"),
                Resolution::Unmatched => {
                    // Still logged: the item may be on another machine
                    // and not synced yet. But it closed nothing here.
                    eprintln!(
                        "No open item matches '{id}' (by id or title); recorded anyway in case it syncs in later. Ids are shown by `mur open`."
                    );
                    std::process::exit(1);
                }
            }
        }
        Some(OpenAction::Mute { origin }) => {
            let mut cfg = mur_common::config::Config::load_or_default(&cfg_path);
            // Record even when nothing currently carries it — a source
            // can be legitimately empty today — but say so, so a typo
            // surfaces here rather than as a silent no-op later.
            let mut seen: Vec<String> = crate::open_items::collect(&home)
                .into_iter()
                .map(|i| i.origin)
                .collect();
            if !seen.contains(&origin) {
                seen.sort();
                seen.dedup();
                eprintln!(
                    "warning: nothing currently has origin '{origin}' (in use: {})",
                    if seen.is_empty() {
                        "none".to_string()
                    } else {
                        seen.join(", ")
                    }
                );
            }
            if !cfg.open_items.muted.contains(&origin) {
                cfg.open_items.muted.push(origin.clone());
                cfg.open_items.muted.sort();
                crate::store::config::save_config_at(&cfg_path, &cfg)?;
            }
            println!("Muted {origin}");
        }
        Some(OpenAction::Unmute { origin }) => {
            let mut cfg = mur_common::config::Config::load_or_default(&cfg_path);
            cfg.open_items.muted.retain(|m| m != &origin);
            crate::store::config::save_config_at(&cfg_path, &cfg)?;
            // Unmuting something that was not muted is not an error:
            // the requested end state holds either way.
            println!("Unmuted {origin}");
        }
        None => {
            let items = crate::open_items::collect(&home);
            // Fail toward showing: an unreadable config yields no
            // mutes, never a quiet, confident, incomplete list.
            let configured = mur_common::config::Config::load_or_default(&cfg_path)
                .open_items
                .muted;
            // `--all` suspends the policy for this render. It does not
            // change what the policy *is*, which is why only the human
            // half below consults it.
            let effective = if all { Vec::new() } else { configured.clone() };
            let (visible, matched) = crate::open_items::partition(items.clone(), &effective);
            if json {
                // Display policy never truncates the machine-readable
                // form: consumers get every item plus the configured
                // mute set and apply their own policy. Emitting only
                // the visible half would make a fully muted list
                // indistinguishable from an empty one, and reporting
                // only the mutes that matched today would make a
                // consumer's view of the policy depend on what happens
                // to be outstanding.
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "items": items,
                        "muted": configured,
                    }))?
                );
            } else {
                let (visible, coverage) = if check {
                    let (v, c) =
                        crate::open_items::probe::annotate(visible, &std::env::current_dir()?);
                    (v, c.line())
                } else {
                    (visible, None)
                };
                let (fresh, stale) = crate::open_items::split_stale(visible, chrono::Utc::now());
                // `--all` suspends this policy too: stale items come
                // back into the list rather than being counted in a
                // footer. Oldest last, which the split already gives.
                let (shown, hidden) = if all {
                    ([fresh, stale].concat(), 0)
                } else {
                    let n = stale.len();
                    (fresh, n)
                };
                print!("{}", crate::open_items::render(&shown, &matched, hidden));
                if let Some(line) = coverage {
                    println!("\n{line}");
                }
            }
        }
    }
    Ok(())
}
