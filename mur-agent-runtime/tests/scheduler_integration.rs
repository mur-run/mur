use mur_agent_runtime::scheduler::next_n_fires;

#[test]
fn next_n_fires_returns_n_times() {
    // "0 * * * *" = every hour. Ask for 3 next fire times.
    let fires = next_n_fires("0 * * * *", 3).expect("should parse");
    assert_eq!(fires.len(), 3);
    // Each successive firing is ~60 minutes after the previous.
    for w in fires.windows(2) {
        let diff_min = (w[1] - w[0]).num_minutes();
        assert!(
            (55..=65).contains(&diff_min),
            "expected ~60 min gap, got {diff_min} min"
        );
    }
}

#[test]
fn next_n_fires_bad_expr_returns_err() {
    assert!(next_n_fires("not a cron", 3).is_err());
}

#[test]
fn next_n_fires_five_field_posix() {
    // "30 9 * * 1-5" = weekday 09:30. Should give 5 results.
    let fires = next_n_fires("30 9 * * 1-5", 5).expect("should parse");
    assert_eq!(fires.len(), 5);
}
