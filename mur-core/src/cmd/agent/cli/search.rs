//! `/search` command: search the local project index from the chat TUI.
use super::*;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;
/// Preview lines per hit in the summary. Three is enough to recognise a chunk
/// without pasting a generated artifact into the transcript.
const DEFAULT_LINES: usize = 3;
const USAGE: &str = "usage: /search <query> [--all] [--limit N] [--lines N] [--send] | /search --expand <id>[,<id>...]";

#[derive(Debug)]
pub(super) enum SearchArgs {
    Query {
        query: String,
        lines: usize,
        limit: usize,
        all: bool,
        send: bool,
    },
    Expand(Vec<usize>),
}

fn parse_args(args: &[String]) -> Result<SearchArgs, String> {
    let mut all = false;
    let mut send = false;
    let mut limit = DEFAULT_LIMIT;
    let mut lines: Option<usize> = None;
    let mut expand: Option<Vec<usize>> = None;
    let mut query_only_flags: Vec<&'static str> = Vec::new();
    let mut query = Vec::new();
    let mut i = 0;
    while i < args.len() {
        // `--limit`/`--lines`/`--expand` each take a value; `take_value` advances
        // `i` past it so the arms below only deal with parsing.
        let take_value = |i: &mut usize, flag: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("search: `{flag}` needs a value"))
        };
        match args[i].as_str() {
            "--all" => {
                all = true;
                query_only_flags.push("--all");
            }
            "--send" => {
                send = true;
                query_only_flags.push("--send");
            }
            "--limit" => {
                let value = take_value(&mut i, "--limit")?;
                match value.parse::<usize>() {
                    Ok(n) if (1..=MAX_LIMIT).contains(&n) => limit = n,
                    _ => return Err(format!("search: `--limit` must be 1–{MAX_LIMIT}")),
                }
                query_only_flags.push("--limit");
            }
            "--lines" => {
                let value = take_value(&mut i, "--lines")?;
                match value.parse::<usize>() {
                    Ok(n) if n >= 1 => lines = Some(n),
                    _ => return Err("search: `--lines` must be >= 1".to_owned()),
                }
            }
            "--expand" => {
                let value = take_value(&mut i, "--expand")?;
                expand = Some(parse_expand_ids(&value).map_err(|e| format!("search: {e}"))?);
            }
            flag if flag.starts_with("--") => {
                return Err(format!("search: unknown option `{flag}`"));
            }
            word => query.push(word),
        }
        i += 1;
    }

    if let Some(ids) = expand {
        if lines.is_some() {
            return Err("search: `--lines` and `--expand` cannot be combined".to_owned());
        }
        if let Some(flag) = query_only_flags.first() {
            return Err(format!(
                "search: `{flag}` cannot be combined with `--expand`"
            ));
        }
        if !query.is_empty() {
            return Err("search: `--expand` takes no query".to_owned());
        }
        return Ok(SearchArgs::Expand(ids));
    }

    let query = query.join(" ");
    if query.is_empty() {
        return Err(USAGE.to_owned());
    }
    Ok(SearchArgs::Query {
        query,
        lines: lines.unwrap_or(DEFAULT_LINES),
        limit,
        all,
        send,
    })
}

pub(super) async fn handle(app: &mut App, args: &[String], tx: &mpsc::Sender<StreamMsg>) {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            app.push_system(message);
            return;
        }
    };

    match parsed {
        SearchArgs::Expand(ids) => {
            let Some(snapshot) = app.search_snapshot.as_ref() else {
                app.push_system("search: no previous search results; run /search <query> first");
                return;
            };
            match render_expanded(snapshot, &ids) {
                Ok(text) => app.push_system(text),
                Err(message) => app.push_system(format!("search: {message}")),
            }
        }
        SearchArgs::Query {
            query,
            lines,
            limit,
            all,
            send,
        } => match crate::cmd::project::do_project_search(&query, None, limit, all).await {
            Ok(result) => {
                let (text, snapshot) = render(&query, result, lines);
                app.search_snapshot = snapshot;
                if send {
                    start_turn(app, text, tx);
                } else {
                    app.push_system(text);
                }
            }
            Err(err) => app.push_system(format!("search: {err:#}")),
        },
    }
}

/// The last `/search` result set, kept so `--expand` can reference it by id
/// without re-running the search or touching the index.
pub(super) struct SearchSnapshot {
    pub query: String,
    pub hits: Vec<crate::cmd::project::ProjectSearchChunk>,
}

/// score DESC, path ASC, line_start ASC, line_end ASC. `total_cmp` never
/// panics; NaN is folded to -inf first so a broken score sorts last rather
/// than to the top (`total_cmp` ranks +NaN above every real number).
fn sort_hits(hits: &mut [crate::cmd::project::ProjectSearchChunk]) {
    fn rank(score: f32) -> f32 {
        if score.is_nan() {
            f32::NEG_INFINITY
        } else {
            score
        }
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

fn render_expanded(snapshot: &SearchSnapshot, ids: &[usize]) -> Result<String, String> {
    use std::fmt::Write as _;
    let total = snapshot.hits.len();
    for &id in ids {
        if id < 1 || id > total {
            return Err(format!("id {id} out of range (1..={total})"));
        }
    }
    let mut out = format!(
        "(from: /search \"{}\" — {total} hits)\n",
        snapshot.query.replace('"', "'")
    );
    for &id in ids {
        let hit = &snapshot.hits[id - 1];
        let _ = write_header(&mut out, id, hit);
        let _ = writeln!(out, "{}", hit.content);
    }
    Ok(out.trim_end().to_owned())
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

/// Sorts, renders the summary, and hands back the snapshot for the caller to
/// park in session state — kept pure so the renderer needs no `App`.
fn render(
    query: &str,
    result: crate::cmd::project::ProjectSearchResult,
    lines: usize,
) -> (String, Option<SearchSnapshot>) {
    if result.chunks.is_empty() {
        return (format!("search: no indexed matches for `{query}`"), None);
    }
    let mut hits = result.chunks;
    sort_hits(&mut hits);
    let body = render_summary(&hits, lines);
    let text = format!(
        "search: {} match(es) for `{query}` — showing {}\n{body}",
        result.total_hits,
        hits.len()
    );
    (
        text,
        Some(SearchSnapshot {
            query: query.to_owned(),
            hits,
        }),
    )
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
    fn lines_and_expand_are_mutually_exclusive() {
        let args = [
            "--lines".to_string(),
            "5".into(),
            "--expand".into(),
            "2".into(),
        ];
        assert_eq!(
            parse_args(&args).unwrap_err(),
            "search: `--lines` and `--expand` cannot be combined"
        );
    }

    #[test]
    fn expand_refuses_a_query() {
        let args = ["--expand".to_string(), "2".into(), "render".into()];
        assert_eq!(
            parse_args(&args).unwrap_err(),
            "search: `--expand` takes no query"
        );
    }

    #[test]
    fn lines_must_be_at_least_one() {
        let args = ["render".to_string(), "--lines".into(), "0".into()];
        assert_eq!(
            parse_args(&args).unwrap_err(),
            "search: `--lines` must be >= 1"
        );
    }

    #[test]
    fn query_defaults_to_three_preview_lines() {
        let args = ["render".to_string(), "chunk".into()];
        match parse_args(&args).unwrap() {
            SearchArgs::Query { query, lines, .. } => {
                assert_eq!(query, "render chunk");
                assert_eq!(lines, DEFAULT_LINES);
            }
            other => panic!("expected a query, got {other:?}"),
        }
    }

    #[test]
    fn expanded_prints_source_header_once_and_full_content() {
        let snapshot = SearchSnapshot {
            query: "render chunk".into(),
            hits: vec![hit("aaa\nbbb\nccc\nddd"), hit("eee\nfff")],
        };
        let out = render_expanded(&snapshot, &[2, 1]).unwrap();
        assert_eq!(out.matches("from: /search").count(), 1, "{out}");
        assert!(out.contains("— 2 hits"), "{out}");
        assert!(out.contains("ddd") && out.contains("fff"), "{out}");
        assert!(!out.contains("… +"), "{out}");
        assert!(out.find("[2]").unwrap() < out.find("[1]").unwrap(), "{out}");
    }

    #[test]
    fn expanded_rejects_out_of_range_id() {
        let snapshot = SearchSnapshot {
            query: "q".into(),
            hits: vec![hit("a")],
        };
        assert_eq!(
            render_expanded(&snapshot, &[3]).unwrap_err(),
            "id 3 out of range (1..=1)"
        );
    }

    #[test]
    fn expand_refuses_query_only_flags() {
        for flag in ["--all", "--send", "--limit"] {
            let mut args = vec!["--expand".to_string(), "2".into(), flag.into()];
            if flag == "--limit" {
                args.push("3".into());
            }
            assert_eq!(
                parse_args(&args).unwrap_err(),
                format!("search: `{flag}` cannot be combined with `--expand`"),
                "{flag} should be rejected in the expand path"
            );
        }
    }

    #[test]
    fn search_then_expand_round_trips_through_the_snapshot() {
        let result = crate::cmd::project::ProjectSearchResult {
            total_hits: 1,
            chunks: vec![hit("one\ntwo\nthree\nfour\nfive")],
        };
        let (summary, snapshot) = render("render", result, 2);
        assert!(summary.contains("… +3 lines (--expand 1)"));

        let snapshot = snapshot.expect("a non-empty search must leave a snapshot");
        let expanded = render_expanded(&snapshot, &[1]).unwrap();
        assert!(expanded.starts_with("(from: /search \"render\" — 1 hits)"));
        assert!(
            expanded.contains("five"),
            "expand shows the untruncated body"
        );
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
        let (text, snapshot) = render(
            "bird",
            crate::cmd::project::ProjectSearchResult {
                chunks: vec![],
                total_hits: 0,
            },
            DEFAULT_LINES,
        );
        assert_eq!(text, "search: no indexed matches for `bird`");
        assert!(snapshot.is_none());
    }
}
