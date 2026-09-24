//! `/search` command: search the local project index from the chat TUI.
use super::*;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;

pub(super) async fn handle(app: &mut App, args: &[String], tx: &mpsc::Sender<StreamMsg>) {
    let mut all = false;
    let mut send = false;
    let mut limit = DEFAULT_LIMIT;
    let mut query = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => all = true,
            "--send" => send = true,
            "--limit" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    app.push_system("search: `--limit` needs a number");
                    return;
                };
                match value.parse::<usize>() {
                    Ok(n) if (1..=MAX_LIMIT).contains(&n) => limit = n,
                    _ => {
                        app.push_system(format!("search: `--limit` must be 1–{MAX_LIMIT}"));
                        return;
                    }
                }
            }
            flag if flag.starts_with("--") => {
                app.push_system(format!("search: unknown option `{flag}`"));
                return;
            }
            word => query.push(word),
        }
        i += 1;
    }
    let query = query.join(" ");
    if query.is_empty() {
        app.push_system("usage: /search <query> [--all] [--limit N] [--send]");
        return;
    }

    match crate::cmd::project::do_project_search(&query, None, limit, all).await {
        Ok(result) => {
            let text = render(&query, result);
            if send {
                start_turn(app, text, tx);
            } else {
                app.push_system(text);
            }
        }
        Err(err) => app.push_system(format!("search: {err:#}")),
    }
}

fn render(query: &str, result: crate::cmd::project::ProjectSearchResult) -> String {
    if result.chunks.is_empty() {
        return format!("search: no indexed matches for `{query}`");
    }
    let mut out = format!("search: {} match(es) for `{query}`\n", result.total_hits);
    for chunk in result.chunks {
        let symbol = chunk.symbol.map(|s| format!(" · {s}")).unwrap_or_default();
        out.push_str(&format!(
            "\n{}:{}{}\n{}\n",
            chunk.file, chunk.line_start, symbol, chunk.content
        ));
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_empty_results() {
        assert_eq!(
            render(
                "bird",
                crate::cmd::project::ProjectSearchResult {
                    chunks: vec![],
                    total_hits: 0
                }
            ),
            "search: no indexed matches for `bird`"
        );
    }
}
