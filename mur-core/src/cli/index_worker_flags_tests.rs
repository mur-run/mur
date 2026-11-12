use super::*;

/// Destructure `mur project index-worker …` into `(rebuild, no_notify, no_hook)`.
fn parse(extra: &[&str]) -> (bool, bool, bool) {
    let argv = [&["mur", "project", "index-worker", "name", "/p"][..], extra].concat();
    let cli = Cli::try_parse_from(argv).unwrap();
    let Commands::Project {
        action:
            crate::cli::actions::ProjectAction::IndexWorker {
                rebuild,
                no_notify,
                no_hook,
                ..
            },
    } = cli.command
    else {
        panic!("not `project index-worker`");
    };
    (rebuild, no_notify, no_hook)
}

/// The CLI background path spawns the worker with neither flag, so both must
/// default to off — a CLI-spawned worker keeps notifying and installing the hook.
#[test]
fn index_worker_flags_default_to_false() {
    assert_eq!(parse(&[]), (false, false, false));
}

#[test]
fn index_worker_parses_no_notify_and_no_hook() {
    assert_eq!(parse(&["--no-notify"]), (false, true, false));
    assert_eq!(parse(&["--no-hook"]), (false, false, true));
    assert_eq!(
        parse(&["--rebuild", "--no-notify", "--no-hook"]),
        (true, true, true)
    );
}
