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

/// score DESC, path ASC, line_start ASC, line_end ASC. `total_cmp` never
/// panics; NaN is folded to -inf first so a broken score sorts last rather
/// than to the top (`total_cmp` ranks +NaN above every real number).
fn sort_hits(hits: &mut [crate::cmd::project::ProjectSearchChunk]) {
    fn rank(score: f32) -> f32 {
        if score.is_nan() { f32::NEG_INFINITY } else { score }
    }
    hits.sort_by(|a, b| {
        rank(b.score)
            .total_cmp(&rank(a.score))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
            .then_with(|| a.line_end.cmp(&b.line_end))
    });
}

/// One line of header + at most `lines` lines of body per hit. `content` is the
/// source of truth for length; `line_start`/`line_end` only describe origin.
fn write_header(
    out: &mut String,
    id: usize,
    hit: &crate::cmd::project::ProjectSearchChunk,
) -> std::fmt::Result {
    use std::fmt::Write as _;
    let symbol = hit
        .symbol
        .as_deref()
        .map(|s| format!(" · {s}"))
        .unwrap_or_default();
    writeln!(
        out,
        "\n[{id}] {}:{}-{} (score {:.2}, {}){symbol}",
        hit.file, hit.line_start, hit.line_end, hit.score, hit.chunk_type
    )
}

fn render_summary(hits: &[crate::cmd::project::ProjectSearchChunk], lines: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (index, hit) in hits.iter().enumerate() {
        let id = index + 1;
        let _ = write_header(&mut out, id, hit);
        for line in hit.content.lines().take(lines) {
            let _ = writeln!(out, "{line}");
        }
        let remaining = hit.content.lines().count().saturating_sub(lines);
        if remaining > 0 {
            let _ = writeln!(out, "… +{remaining} lines (--expand {id})");
        }
    }
    out.trim_end().to_owned()
}

fn parse_expand_ids(raw: &str) -> Result<Vec<usize>, String> {
    let mut ids = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        match part.parse::<usize>() {
            Ok(n) if n >= 1 => {
                if !ids.contains(&n) {
                    ids.push(n);
                }
            }
            _ => return Err(format!("`{part}` is not a result id")),
        }
    }
    if ids.is_empty() {
        return Err("no ids given".to_owned());
    }
    Ok(ids)
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
    fn expand_ids_dedup_preserves_input_order() {
        assert_eq!(parse_expand_ids("7,3,7,9,3").unwrap(), vec![7, 3, 9]);
    }

    fn hit(content: &str) -> crate::cmd::project::ProjectSearchChunk {
        crate::cmd::project::ProjectSearchChunk {
            project: "mur".into(),
            file: "src/search.rs".into(),
            language: "rust".into(),
            chunk_type: "fn".into(),
            symbol: None,
            content: content.into(),
            line_start: 120,
            line_end: 158,
            score: 0.83,
        }
    }

    #[test]
    fn sort_is_deterministic_and_nan_safe() {
        let mut a = hit("a");
        a.score = f32::NAN;
        a.file = "z.rs".into();
        let mut b = hit("b");
        b.score = 0.5;
        b.file = "b.rs".into();
        let mut c = hit("c");
        c.score = 0.9;
        c.file = "c.rs".into();
        let mut d = hit("d");
        d.score = 0.9;
        d.file = "a.rs".into();

        let mut hits = vec![a, b, c, d];
        sort_hits(&mut hits);
        let order: Vec<&str> = hits.iter().map(|h| h.file.as_str()).collect();
        assert_eq!(order, vec!["a.rs", "c.rs", "b.rs", "z.rs"]);
    }

    #[test]
    fn preview_truncates_and_points_at_expand() {
        let body = (1..=15)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = render_summary(&[hit(&body)], 3);
        assert!(out.contains("line3"), "{out}");
        assert!(!out.contains("line4"), "{out}");
        assert!(out.contains("… +12 lines (--expand 1)"), "{out}");
    }

    #[test]
    fn preview_shorter_than_limit_has_no_tail() {
        let out = render_summary(&[hit("only\ntwo")], 3);
        assert!(out.contains("two"), "{out}");
        assert!(!out.contains("--expand"), "{out}");
    }

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
