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
    let smoke_passed = Command::new("cargo")
        .args(["test", "--manifest-path"])
        .arg(manifest)
        .args(["--lib", "--quiet"])
        .status()
        .is_ok_and(|status| status.success());

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
    let body = format!(
        concat!(
            "{{\n",
            "  \"schema_version\": 1,\n",
            "  \"requested_suite\": \"{}\",\n",
            "  \"evidence_level\": \"abstract_smoke\",\n",
            "  \"environment\": {{\"os\": \"{}\", \"arch\": \"{}\"}},\n",
            "  \"tests_run\": 15,\n",
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
        suite,
        env::consts::OS,
        env::consts::ARCH,
        usize::from(!smoke_passed),
        smoke_passed,
        gates
    );
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
