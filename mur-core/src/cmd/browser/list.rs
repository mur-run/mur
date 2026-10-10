//! `mur browser list` — a read-only table of recorded runs.
//!
//! Everything here is derived from `actions.yaml` on every call; nothing is
//! cached or written. The pieces are split so each is testable without a
//! runs directory: [`load_rows`] (I/O), [`apply`] (filter + sort) and
//! [`render`] (formatting) are separate functions over plain [`Row`]s.

use std::{fs, path::Path};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use mur_browser::{
    paths,
    recorder::{Mode, Run, from_yaml},
    summary::frecency_score,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use super::runs_dir;

/// Printed (to stderr) when there is nothing recorded yet. The flag is
/// `--run`; a bare positional does not parse.
const EMPTY_HINT: &str = "no recordings — mur browser record --run <name>";
/// Printed (to stderr) when recordings exist but the filters exclude them all.
const NO_MATCH_HINT: &str = "no recordings match the given filters";
/// Placeholder for an empty cell in the table and TSV views.
const EMPTY_CELL: &str = "-";
const COLUMN_GAP: &str = "  ";
const HOURS_SUFFIX: char = 'h';
const DAYS_SUFFIX: char = 'd';
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Order of the listing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Sort {
    /// Alphabetical by name.
    #[default]
    Name,
    /// Newest recording first.
    Recent,
    /// Most-and-latest replayed first; never-replayed runs follow, newest first.
    Frecency,
}

/// Output shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Table,
    Tsv,
    /// Names only, one per line.
    Oneline,
    Json,
}

/// Everything `mur browser list` was asked for, straight from the CLI flags.
#[derive(Debug, Default)]
pub struct ListOpts {
    pub format: Format,
    pub no_header: bool,
    pub tags: Vec<String>,
    pub profile: Option<String>,
    pub grep: Option<String>,
    pub since: Option<String>,
    pub sort: Sort,
}

/// One line of the listing: a flattened, display-ready view of a [`Run`].
/// An unreadable recording keeps its name and carries `error` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub name: String,
    pub description: Option<String>,
    /// One-line label: the description, else the derived intents.
    pub summary: String,
    pub domain: Option<String>,
    pub tags: Vec<String>,
    pub steps: usize,
    pub profile: Option<String>,
    /// `None` only for an unreadable recording.
    pub mode: Option<Mode>,
    /// `None` only for an unreadable recording.
    pub recorded_at: Option<DateTime<Utc>>,
    pub replayed_at: Option<DateTime<Utc>>,
    pub replay_count: u32,
    pub error: Option<String>,
    /// Full, untruncated text `--grep` and the picker match against.
    #[serde(skip_serializing, default)]
    pub search_text: String,
}

impl From<(&str, Result<Run>)> for Row {
    fn from((name, run): (&str, Result<Run>)) -> Self {
        match run {
            Ok(run) => Row {
                name: name.to_owned(),
                summary: run.summary(),
                domain: run.domain(),
                search_text: run.search_text(),
                steps: run.steps.len(),
                description: run.description,
                tags: run.tags,
                profile: run.profile,
                mode: Some(run.mode),
                recorded_at: Some(run.recorded_at),
                replayed_at: run.replayed_at,
                replay_count: run.replay_count,
                error: None,
            },
            Err(error) => Row {
                name: name.to_owned(),
                description: None,
                summary: String::new(),
                domain: None,
                tags: Vec::new(),
                steps: 0,
                profile: None,
                mode: None,
                recorded_at: None,
                replayed_at: None,
                replay_count: 0,
                error: Some(format!("{error:#}")),
                search_text: String::new(),
            },
        }
    }
}

impl Row {
    /// Same score `Run::frecency` gives; an unreadable row sinks to the bottom.
    pub fn frecency(&self, now: DateTime<Utc>) -> f64 {
        match self.recorded_at {
            Some(recorded) => frecency_score(recorded, self.replayed_at, self.replay_count, now),
            None => f64::NEG_INFINITY,
        }
    }
}

/// Which rows to keep. All set conditions must hold; an unreadable row has
/// nothing to match against, so any active filter drops it.
#[derive(Debug, Default)]
pub struct Filter {
    /// Row must carry every one of these tags.
    pub tags: Vec<String>,
    pub profile: Option<String>,
    pub grep: Option<Regex>,
    /// Keep runs recorded at or after this instant.
    pub since: Option<DateTime<Utc>>,
}

impl Filter {
    fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.profile.is_none()
            && self.grep.is_none()
            && self.since.is_none()
    }

    fn keeps(&self, row: &Row) -> bool {
        if self.is_empty() {
            return true;
        }
        let Some(recorded_at) = row.recorded_at else {
            return false;
        };
        self.tags.iter().all(|t| row.tags.contains(t))
            && self
                .profile
                .as_ref()
                .is_none_or(|p| row.profile.as_ref() == Some(p))
            && self
                .grep
                .as_ref()
                .is_none_or(|re| re.is_match(&row.search_text))
            && self.since.is_none_or(|since| recorded_at >= since)
    }

    /// Build a filter from raw flag values, rejecting a bad regex or `--since`.
    pub fn from_opts(opts: &ListOpts, now: DateTime<Utc>) -> Result<Self> {
        let grep = opts
            .grep
            .as_deref()
            .map(|g| Regex::new(g).map_err(|e| anyhow!("invalid --grep {g:?}: {e}")))
            .transpose()?;
        let since = opts
            .since
            .as_deref()
            .map(|s| parse_since(s, now))
            .transpose()?;
        Ok(Filter {
            tags: opts.tags.clone(),
            profile: opts.profile.clone(),
            grep,
            since,
        })
    }
}

/// `7d` / `24h` (relative to `now`) or `YYYY-MM-DD` (midnight UTC).
pub fn parse_since(s: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = s.trim();
    let invalid = || {
        anyhow!(
            "invalid --since {s:?}: expected a duration like 7d or 24h, or a date like 2026-10-01"
        )
    };
    let relative = |suffix: char, unit: fn(i64) -> Duration| -> Option<Result<DateTime<Utc>>> {
        let n: u32 = s.strip_suffix(suffix)?.parse().ok()?;
        Some(
            now.checked_sub_signed(unit(i64::from(n)))
                .ok_or_else(invalid),
        )
    };
    if let Some(parsed) = relative(DAYS_SUFFIX, Duration::days) {
        return parsed;
    }
    if let Some(parsed) = relative(HOURS_SUFFIX, Duration::hours) {
        return parsed;
    }
    NaiveDate::parse_from_str(s, DATE_FORMAT)
        .map(|d| d.and_time(NaiveTime::MIN).and_utc())
        .map_err(|_| invalid())
}

/// Read every run under `runs`. Read-only; a recording that cannot be read or
/// parsed becomes a [`Row`] with `error` set — it never aborts the listing.
pub fn load_rows(runs: &Path) -> Vec<Row> {
    let Ok(entries) = fs::read_dir(runs) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.file_name()))
        .filter_map(|name| name.into_string().ok())
        .filter(|name| paths::validate_name(name).is_ok())
        .map(|name| {
            let file = runs.join(&name).join(paths::ACTIONS_FILE);
            let run = fs::read_to_string(&file)
                .map_err(|e| anyhow!("read {}: {e}", file.display()))
                .and_then(|yaml| from_yaml(&yaml));
            Row::from((name.as_str(), run))
        })
        .collect()
}

/// Filter, then order. Ties always fall back to name so output is stable.
pub fn apply(rows: Vec<Row>, filter: &Filter, sort: Sort, now: DateTime<Utc>) -> Vec<Row> {
    let mut rows: Vec<Row> = rows.into_iter().filter(|r| filter.keeps(r)).collect();
    match sort {
        Sort::Name => rows.sort_by(|a, b| a.name.cmp(&b.name)),
        Sort::Recent => rows.sort_by(|a, b| {
            b.recorded_at
                .cmp(&a.recorded_at)
                .then_with(|| a.name.cmp(&b.name))
        }),
        Sort::Frecency => rows.sort_by(|a, b| {
            b.frecency(now)
                .total_cmp(&a.frecency(now))
                .then_with(|| a.name.cmp(&b.name))
        }),
    }
    rows
}

/// Format `rows`. `header` applies to the table and TSV views only.
pub fn render(rows: &[Row], format: Format, header: bool) -> String {
    match format {
        Format::Json => serde_json::to_string_pretty(rows).unwrap_or_else(|_| "[]".to_owned()),
        Format::Oneline => rows
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Format::Tsv => render_tsv(rows, header),
        Format::Table => render_table(rows, header),
    }
}

fn columns(rows: &[Row]) -> Vec<&'static str> {
    let mut cols = vec!["NAME", "SUMMARY", "DOMAIN"];
    if rows.iter().any(|r| !r.tags.is_empty()) {
        cols.push("TAGS");
    }
    cols.extend(["STEPS", "MODE", "RUNS", "RECORDED"]);
    cols
}

/// One row's cells, in the order [`columns`] declares them.
fn cells(row: &Row, with_tags: bool) -> Vec<String> {
    let or_dash = |s: String| {
        if s.is_empty() {
            EMPTY_CELL.to_owned()
        } else {
            s
        }
    };
    let mut out = vec![
        one_line(&row.name),
        or_dash(one_line(&row.summary)),
        or_dash(row.domain.clone().unwrap_or_default()),
    ];
    if with_tags {
        out.push(or_dash(row.tags.join(",")));
    }
    out.push(row.steps.to_string());
    out.push(row.mode.map(mode_label).unwrap_or(EMPTY_CELL).to_owned());
    out.push(row.replay_count.to_string());
    out.push(
        row.recorded_at
            .map(|t| t.format(DATE_FORMAT).to_string())
            .unwrap_or_else(|| EMPTY_CELL.to_owned()),
    );
    out
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Test => "test",
        Mode::Automation => "automation",
        Mode::Live => "live",
    }
}

/// Collapse tabs/newlines so a multi-line description cannot break a row.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unreadable_label(row: &Row) -> String {
    format!(
        "{} (unreadable: {})",
        one_line(&row.name),
        one_line(row.error.as_deref().unwrap_or_default())
    )
}

fn render_tsv(rows: &[Row], header: bool) -> String {
    let with_tags = columns(rows).contains(&"TAGS");
    let mut lines = Vec::new();
    if header {
        lines.push(columns(rows).join("\t"));
    }
    for row in rows {
        let mut c = cells(row, with_tags);
        if row.error.is_some() {
            c[1] = unreadable_label(row);
        }
        lines.push(c.join("\t"));
    }
    lines.join("\n")
}

fn render_table(rows: &[Row], header: bool) -> String {
    let cols = columns(rows);
    let with_tags = cols.contains(&"TAGS");
    let body: Vec<Vec<String>> = rows.iter().map(|r| cells(r, with_tags)).collect();
    // Widths come from readable rows only: one long error must not stretch
    // every column.
    let mut widths: Vec<usize> = cols.iter().map(|c| c.width()).collect();
    for (row, cells) in rows.iter().zip(&body) {
        if row.error.is_none() {
            for (w, cell) in widths.iter_mut().zip(cells) {
                *w = (*w).max(cell.width());
            }
        }
    }
    let line = |cells: &[String]| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w.saturating_sub(c.width()))))
            .collect();
        padded.join(COLUMN_GAP).trim_end().to_owned()
    };
    let mut lines = Vec::new();
    if header {
        let head: Vec<String> = cols.iter().map(|c| (*c).to_owned()).collect();
        lines.push(line(&head));
    }
    for (row, cells) in rows.iter().zip(&body) {
        lines.push(if row.error.is_some() {
            unreadable_label(row)
        } else {
            line(cells)
        });
    }
    lines.join("\n")
}

/// `mur browser list`: load, filter, sort, print.
pub fn list(opts: ListOpts) -> Result<()> {
    let now = Utc::now();
    let filter = Filter::from_opts(&opts, now)?;
    let runs = runs_dir()?;
    let all = if runs.exists() {
        load_rows(&runs)
    } else {
        Vec::new()
    };
    let total = all.len();
    let rows = apply(all, &filter, opts.sort, now);
    if rows.is_empty() {
        eprintln!(
            "{}",
            if total == 0 {
                EMPTY_HINT
            } else {
                NO_MATCH_HINT
            }
        );
        if opts.format == Format::Json {
            println!("[]");
        }
        return Ok(());
    }
    println!("{}", render(&rows, opts.format, !opts.no_header));
    Ok(())
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
