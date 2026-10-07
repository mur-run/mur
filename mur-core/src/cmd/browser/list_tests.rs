//! Tests for `mur browser list`: filter/sort on in-memory rows, rendering,
//! and `load_rows` over a real temp directory.

use super::*;
use mur_browser::recorder::{Action, Step, to_yaml};

fn now() -> DateTime<Utc> {
    "2026-10-10T12:00:00Z".parse().unwrap()
}

fn step(n: u32, intent: &str, action: Action, value: Option<&str>) -> Step {
    Step {
        step: n,
        intent: intent.to_owned(),
        intent_auto: false,
        action,
        value: value.map(str::to_owned),
        locators: vec!["role=button".to_owned()],
        healed: false,
        last_hit: 0,
        ref_at_record: None,
    }
}

fn run(name: &str, age_days: i64) -> Run {
    Run {
        name: name.to_owned(),
        mode: Mode::Test,
        profile: None,
        recorded_at: now() - Duration::days(age_days),
        description: None,
        tags: Vec::new(),
        replayed_at: None,
        replay_count: 0,
        steps: vec![
            step(
                1,
                "open the site",
                Action::Goto,
                Some("https://shop.example.com/"),
            ),
            step(2, "click the buy button", Action::Click, None),
        ],
    }
}

fn row(run: Run) -> Row {
    Row::from((run.name.clone().as_str(), Ok(run)))
}

fn broken(name: &str, why: &str) -> Row {
    Row::from((name, Err(anyhow!(why.to_owned()))))
}

fn names(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|r| r.name.as_str()).collect()
}

fn opts() -> ListOpts {
    ListOpts::default()
}

#[test]
fn sort_by_name_is_alphabetical() {
    let rows = vec![
        row(run("zeta", 1)),
        row(run("alpha", 9)),
        row(run("mid", 5)),
    ];
    let out = apply(rows, &Filter::default(), Sort::Name, now());
    assert_eq!(names(&out), ["alpha", "mid", "zeta"]);
}

#[test]
fn sort_by_recent_puts_newest_first() {
    let rows = vec![row(run("old", 30)), row(run("new", 1)), row(run("mid", 5))];
    let out = apply(rows, &Filter::default(), Sort::Recent, now());
    assert_eq!(names(&out), ["new", "mid", "old"]);
}

#[test]
fn sort_by_frecency_puts_replayed_first_then_newest() {
    let mut hot = run("hot", 30);
    hot.replay_count = 5;
    hot.replayed_at = Some(now() - Duration::days(1));
    let rows = vec![
        row(run("unreplayed-old", 20)),
        row(hot),
        row(run("unreplayed-new", 2)),
        broken("broken", "bad yaml"),
    ];
    let out = apply(rows, &Filter::default(), Sort::Frecency, now());
    assert_eq!(
        names(&out),
        ["hot", "unreplayed-new", "unreplayed-old", "broken"]
    );
}

#[test]
fn tag_filter_requires_every_tag() {
    let mut a = run("a", 1);
    a.tags = vec!["smoke".into(), "checkout".into()];
    let mut b = run("b", 1);
    b.tags = vec!["smoke".into()];
    let filter = Filter {
        tags: vec!["smoke".into(), "checkout".into()],
        ..Filter::default()
    };
    let out = apply(vec![row(a), row(b)], &filter, Sort::Name, now());
    assert_eq!(names(&out), ["a"]);
}

#[test]
fn profile_filter_matches_exactly() {
    let mut a = run("a", 1);
    a.profile = Some("work".into());
    let mut b = run("b", 1);
    b.profile = Some("homework".into());
    let filter = Filter {
        profile: Some("work".into()),
        ..Filter::default()
    };
    let out = apply(
        vec![row(a), row(b), row(run("c", 1))],
        &filter,
        Sort::Name,
        now(),
    );
    assert_eq!(names(&out), ["a"]);
}

#[test]
fn since_filter_keeps_recent_and_combines_with_tags() {
    let mut recent = run("recent", 2);
    recent.tags = vec!["t".into()];
    let mut recent_untagged = run("recent-untagged", 2);
    recent_untagged.tags = Vec::new();
    let mut old = run("old", 40);
    old.tags = vec!["t".into()];
    let filter = Filter {
        tags: vec!["t".into()],
        since: Some(parse_since("7d", now()).unwrap()),
        ..Filter::default()
    };
    let out = apply(
        vec![row(recent), row(recent_untagged), row(old)],
        &filter,
        Sort::Name,
        now(),
    );
    assert_eq!(names(&out), ["recent"]);
}

#[test]
fn any_filter_drops_an_unreadable_row_but_no_filter_keeps_it() {
    let rows = || vec![row(run("ok", 1)), broken("bad", "nope")];
    let kept = apply(rows(), &Filter::default(), Sort::Name, now());
    assert_eq!(names(&kept), ["bad", "ok"]);
    let filter = Filter {
        profile: Some("x".into()),
        ..Filter::default()
    };
    assert!(apply(rows(), &filter, Sort::Name, now()).is_empty());
}

#[test]
fn grep_matches_truncated_intent() {
    // The summary is cut at 40 columns; the match sits past the cut.
    let mut r = run("long", 1);
    r.steps = vec![
        step(
            1,
            "open the site",
            Action::Goto,
            Some("https://shop.example.com/"),
        ),
        step(
            2,
            "scroll through the entire product catalogue slowly then apply the zanzibar coupon",
            Action::Click,
            None,
        ),
    ];
    let r = row(r);
    assert!(!r.summary.contains("zanzibar"), "summary: {}", r.summary);
    let filter = Filter {
        grep: Some(Regex::new("zanzibar").unwrap()),
        ..Filter::default()
    };
    let out = apply(vec![r, row(run("other", 1))], &filter, Sort::Name, now());
    assert_eq!(names(&out), ["long"]);
}

#[test]
fn grep_also_matches_name_domain_and_tags() {
    let mut a = run("checkout-flow", 1);
    a.tags = vec!["regression".into()];
    let grep = |re: &str| Filter {
        grep: Some(Regex::new(re).unwrap()),
        ..Filter::default()
    };
    for hit in ["checkout", "shop\\.example", "regress"] {
        let out = apply(vec![row(a.clone())], &grep(hit), Sort::Name, now());
        assert_eq!(out.len(), 1, "{hit} should match");
    }
}

#[test]
fn bad_grep_is_rejected_naming_the_flag() {
    let o = ListOpts {
        grep: Some("(".into()),
        ..opts()
    };
    let err = Filter::from_opts(&o, now()).unwrap_err().to_string();
    assert!(err.contains("--grep"), "{err}");
}

#[test]
fn since_parses_hours_days_and_dates() {
    assert_eq!(
        parse_since("24h", now()).unwrap(),
        now() - Duration::hours(24)
    );
    assert_eq!(parse_since("7d", now()).unwrap(), now() - Duration::days(7));
    assert_eq!(
        parse_since("2026-10-01", now()).unwrap(),
        "2026-10-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[test]
fn since_parse_error_says_what_is_accepted() {
    for bad in ["", "7", "-3d", "yesterday", "2026-13-40", "7w"] {
        let err = parse_since(bad, now()).unwrap_err().to_string();
        assert!(err.contains("--since"), "{bad}: {err}");
        assert!(
            err.contains("7d") && err.contains("2026-10-01"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn oneline_is_sorted_names_joined_by_newline() {
    let rows = vec![row(run("b", 1)), row(run("a", 1)), row(run("c", 1))];
    let sorted = apply(rows, &Filter::default(), Sort::Name, now());
    assert_eq!(render(&sorted, Format::Oneline, true), "a\nb\nc");
}

#[test]
fn json_round_trips_and_leaves_search_text_out() {
    let mut r = run("one", 1);
    r.description = Some("buy something".into());
    r.tags = vec!["smoke".into()];
    r.replay_count = 3;
    let rows = vec![row(r), broken("bad", "boom")];
    let json = render(&rows, Format::Json, true);
    assert!(!json.contains("search_text"));
    let back: Vec<Row> = serde_json::from_str(&json).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].name, "one");
    assert_eq!(back[0].summary, "buy something");
    assert_eq!(back[0].domain.as_deref(), Some("shop.example.com"));
    assert_eq!(back[0].tags, ["smoke"]);
    assert_eq!(back[0].steps, 2);
    assert_eq!(back[0].replay_count, 3);
    assert_eq!(back[1].error.as_deref(), Some("boom"));
}

#[test]
fn unreadable_row_renders_name_with_reason() {
    let rows = vec![row(run("good", 1)), broken("bad", "invalid yaml at line 3")];
    let table = render(&rows, Format::Table, true);
    assert!(
        table
            .lines()
            .any(|l| l == "bad (unreadable: invalid yaml at line 3)"),
        "{table}"
    );
    let tsv = render(&rows, Format::Tsv, true);
    assert!(
        tsv.contains("bad (unreadable: invalid yaml at line 3)"),
        "{tsv}"
    );
}

#[test]
fn a_long_error_does_not_widen_the_columns() {
    let plain = render(&[row(run("good", 1))], Format::Table, true);
    let mixed = render(
        &[row(run("good", 1)), broken("bad", &"x".repeat(200))],
        Format::Table,
        true,
    );
    assert_eq!(plain.lines().next(), mixed.lines().next());
}

#[test]
fn tags_column_appears_only_when_a_row_has_tags() {
    let without = render(&[row(run("a", 1))], Format::Table, true);
    assert!(!without.contains("TAGS"));
    let mut tagged = run("b", 1);
    tagged.tags = vec!["smoke".into(), "ui".into()];
    let with = render(&[row(run("a", 1)), row(tagged)], Format::Table, true);
    assert!(with.lines().next().unwrap().contains("TAGS"));
    assert!(with.contains("smoke,ui"));
}

#[test]
fn no_header_drops_the_header_line() {
    let rows = vec![row(run("a", 1))];
    let table = render(&rows, Format::Table, false);
    assert_eq!(table.lines().count(), 1);
    assert!(!table.contains("NAME"));
    let tsv = render(&rows, Format::Tsv, false);
    assert!(!tsv.contains("NAME"));
}

#[test]
fn tsv_has_one_tab_separated_line_per_row_with_equal_columns() {
    let mut d = run("with\ttab", 1);
    d.description = Some("two\nlines\tand a tab".into());
    let tsv = render(&[row(run("a", 1)), row(d)], Format::Tsv, true);
    let counts: Vec<usize> = tsv.lines().map(|l| l.matches('\t').count()).collect();
    assert!(counts.iter().all(|c| *c == counts[0]), "{tsv}");
    assert_eq!(counts.len(), 3);
}

#[test]
fn table_pads_by_display_width_for_cjk() {
    let mut cjk = run("zh", 1);
    cjk.description = Some("點擊登入按鈕".into());
    let table = render(&[row(cjk), row(run("en", 1))], Format::Table, true);
    // Every line's DOMAIN column must start at the same display column.
    let starts: Vec<usize> = table
        .lines()
        .skip(1)
        .map(|l| l.find("shop.example.com").map(|b| l[..b].width()).unwrap())
        .collect();
    assert_eq!(starts[0], starts[1], "{table}");
}

#[test]
fn load_rows_reads_good_runs_and_survives_bad_ones() {
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path();
    let write = |name: &str, body: &str| {
        let d = runs.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(paths::ACTIONS_FILE), body).unwrap();
    };
    write("good", &to_yaml(&run("good", 1)).unwrap());
    write("garbled", "not: [valid");
    fs::create_dir_all(runs.join("empty-dir")).unwrap();
    fs::write(runs.join("stray-file.txt"), "x").unwrap();

    let rows = apply(load_rows(runs), &Filter::default(), Sort::Name, now());
    assert_eq!(names(&rows), ["empty-dir", "garbled", "good"]);
    assert!(rows[0].error.is_some());
    assert!(rows[1].error.is_some());
    assert!(rows[2].error.is_none());
    assert_eq!(rows[2].steps, 2);
}

#[test]
fn load_rows_on_a_missing_dir_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_rows(&dir.path().join("nope")).is_empty());
}

#[test]
fn load_rows_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("good");
    fs::create_dir_all(&d).unwrap();
    let file = d.join(paths::ACTIONS_FILE);
    fs::write(&file, to_yaml(&run("good", 1)).unwrap()).unwrap();
    let before = fs::read(&file).unwrap();
    let _ = load_rows(dir.path());
    assert_eq!(fs::read(&file).unwrap(), before);
}
