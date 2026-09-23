use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const GATES: &[&str] = &[
    "SM",
    "CRASH",
    "LINEAR",
    "HW",
    "CRYPTO",
    "MEM",
    "EXPOSURE",
    "MATERIALIZE",
    "DISCOVERY",
    "METADATA",
    "TIMING",
    "ISOLATION",
    "CANARY",
    "CONFORMANCE",
    "PERF",
];

fn usage() -> ! {
    eprintln!("usage: capsule-g0 --suite <abstract-smoke|all> --report <path>");
    std::process::exit(64);
}

/// Count the tests libtest actually ran, by summing every `test result:` line.
///
/// The report must not state a test count that nobody measured: a hard-coded
/// number silently drifts every time a gate adds cases, and a report that
/// misstates its own size is not evidence.
fn count_tests_run(output: &str) -> Option<usize> {
    let mut total = None;
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix("test result:") else {
            continue;
        };
        let Some(passed) = rest.split_whitespace().nth(1) else {
            continue;
        };
        let Ok(passed) = passed.parse::<usize>() else {
            continue;
        };
        let failed = rest
            .split_whitespace()
            .skip_while(|word| *word != "passed;")
            .nth(1)
            .and_then(|word| word.parse::<usize>().ok())
            .unwrap_or(0);
        total = Some(total.unwrap_or(0) + passed + failed);
    }
    total
}

/// The evidence envelope. Pure so the contract fields can be pinned by tests.
fn render_report(suite: &str, tests_run: &str, smoke_passed: bool) -> String {
    let gates = GATES
        .iter()
        .map(|gate| {
            let status = if *gate == "SM" {
                "incomplete"
            } else {
                "not_run"
            };
            format!("    \"G0-{gate}\": \"{status}\"")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        concat!(
            "{{\n",
            "  \"schema_version\": \"{}\",\n",
            "  \"requested_suite\": \"{}\",\n",
            "  \"evidence_level\": \"abstract_smoke\",\n",
            "  \"environment\": {{\"os\": \"{}\", \"arch\": \"{}\"}},\n",
            "  \"tests_run\": {},\n",
            "  \"failures\": {},\n",
            "  \"smoke_passed\": {},\n",
            "  \"gates\": {{\n{}\n  }},\n",
            "  \"g0_status\": \"incomplete\",\n",
            "  \"strict_supported\": false,\n",
            "  \"g1_authorized\": false,\n",
            "  \"g2_authorized\": false,\n",
            "  \"limits\": [\n",
            "    \"Atomic nonrollback anchor is assumed, not implemented.\",\n",
            "    \"No real storage, cryptography, OS isolation or MUR code is tested.\",\n",
            "    \"Selected traces only; not exhaustive bounded model checking.\",\n",
            "    \"The all suite fails closed until every required gate is implemented.\"\n",
            "  ]\n",
            "}}\n"
        ),
        capsule_g0_validation::ENVELOPE_SCHEMA,
        suite,
        env::consts::OS,
        env::consts::ARCH,
        tests_run,
        usize::from(!smoke_passed),
        smoke_passed,
        gates
    )
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let mut suite = None;
    let mut report = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--suite" => suite = args.next(),
            "--report" => report = args.next().map(PathBuf::from),
            _ => usage(),
        }
    }
    let suite = suite.unwrap_or_else(|| usage());
    if !matches!(suite.as_str(), "abstract-smoke" | "all") {
        usage();
    }
    let report = report.unwrap_or_else(|| usage());

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let smoke = Command::new("cargo")
        .args(["test", "--manifest-path"])
        .arg(manifest)
        .args(["--lib", "--quiet"])
        .output();
    let (smoke_passed, tests_run) = match &smoke {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            print!("{text}");
            (output.status.success(), count_tests_run(&text))
        }
        Err(error) => {
            eprintln!("cannot run the library suite: {error}");
            (false, None)
        }
    };
    // A report that cannot measure its own suite must say so, not guess.
    let tests_run = tests_run
        .map(|count| count.to_string())
        .unwrap_or_else(|| "null".to_owned());

    let body = render_report(&suite, &tests_run, smoke_passed);
    if let Some(parent) = report.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        eprintln!("cannot create report directory: {error}");
        return ExitCode::from(1);
    }
    if let Err(error) = fs::write(&report, body) {
        eprintln!("cannot write report: {error}");
        return ExitCode::from(1);
    }
    println!("Evidence report: {}", report.display());
    if !smoke_passed {
        ExitCode::from(1)
    } else if suite == "all" {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::{count_tests_run, render_report};

    #[test]
    fn counts_every_libtest_result_line() {
        let output = concat!(
            "running 47 tests\n",
            "test result: ok. 47 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        assert_eq!(count_tests_run(output), Some(47));
    }

    #[test]
    fn sums_multiple_suites_and_counts_failures() {
        let output = concat!(
            "test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
            "test result: FAILED. 30 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        assert_eq!(count_tests_run(output), Some(47));
    }

    #[test]
    fn absent_result_line_yields_no_count_rather_than_zero() {
        assert_eq!(count_tests_run("error: could not compile\n"), None);
    }

    /// Contract :203 — the envelope carries the fixed identifier
    /// `capsule-envelope-g0-v1`, never a bare number that could be confused
    /// with a future production v1 (§5 answer 3). Same constant the model
    /// uses, so the report and `Model` cannot drift apart.
    #[test]
    fn report_uses_fixed_envelope_schema_identifier() {
        let body = render_report("abstract-smoke", "69", true);
        let expected = format!(
            "  \"schema_version\": \"{}\",\n",
            capsule_g0_validation::ENVELOPE_SCHEMA
        );
        assert!(body.contains(&expected), "envelope:\n{body}");
        assert!(!body.contains("\"schema_version\": 1"), "envelope:\n{body}");
    }
}
