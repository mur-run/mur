use super::*;
use std::fs;

#[test]
fn test_detect_language_rust() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
    assert_eq!(detect_language(dir.path()), Some(Language::Rust));
}

#[test]
fn test_detect_language_swift() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Package.swift"),
        "// swift-tools-version:5.9",
    )
    .unwrap();
    assert_eq!(detect_language(dir.path()), Some(Language::Swift));
}

#[test]
fn test_detect_language_js() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("package.json"), "{}").unwrap();
    assert_eq!(detect_language(dir.path()), Some(Language::JavaScript));
}

#[test]
fn test_detect_language_ts() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("package.json"), "{}").unwrap();
    fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
    assert_eq!(detect_language(dir.path()), Some(Language::TypeScript));
}

#[test]
fn test_detect_language_python() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();
    assert_eq!(detect_language(dir.path()), Some(Language::Python));
}

#[test]
fn test_detect_language_go() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("go.mod"), "module example.com/test").unwrap();
    assert_eq!(detect_language(dir.path()), Some(Language::Go));
}

#[test]
fn test_detect_language_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(detect_language(dir.path()), None);
}

#[test]
fn test_extract_cargo_deps() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\nserde = { version = \"1\", features = [\"derive\"] }\nanyhow = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n",
        ).unwrap();
    let deps = extract_cargo_deps(dir.path());
    assert!(deps.contains(&"tokio".to_string()));
    assert!(deps.contains(&"serde".to_string()));
    assert!(deps.contains(&"anyhow".to_string()));
    assert!(deps.contains(&"tempfile".to_string()));
}

#[test]
fn test_extract_npm_deps() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"^18","next":"^14"},"devDependencies":{"typescript":"^5","vitest":"^1"}}"#,
        ).unwrap();
    let deps = extract_npm_deps(dir.path());
    assert!(deps.contains(&"react".to_string()));
    assert!(deps.contains(&"next".to_string()));
    assert!(deps.contains(&"typescript".to_string()));
    assert!(deps.contains(&"vitest".to_string()));
}

#[test]
fn test_extract_python_deps_requirements() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("requirements.txt"),
        "fastapi>=0.100\npandas==2.0\n# comment\npytest\n",
    )
    .unwrap();
    let deps = extract_python_deps(dir.path());
    assert!(deps.contains(&"fastapi".to_string()));
    assert!(deps.contains(&"pandas".to_string()));
    assert!(deps.contains(&"pytest".to_string()));
}

#[test]
fn test_extract_go_deps() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
            dir.path().join("go.mod"),
            "module example.com/test\n\ngo 1.21\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgithub.com/spf13/cobra v1.7.0\n)\n",
        ).unwrap();
    let deps = extract_go_deps(dir.path());
    assert!(deps.contains(&"gin".to_string()));
    assert!(deps.contains(&"cobra".to_string()));
}

#[test]
fn test_generate_patterns_for_rust_project() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\nserde = \"1\"\n",
    )
    .unwrap();
    let existing = HashSet::new();
    let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
    assert!(!patterns.is_empty());
    let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"rust-async-runtime-tokio"));
    assert!(names.contains(&"rust-serialization-serde"));
    // Should NOT contain patterns for deps not in the project
    assert!(!names.contains(&"rust-web-axum"));
}

#[test]
fn test_generate_patterns_skips_existing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\n",
    )
    .unwrap();
    let mut existing = HashSet::new();
    existing.insert("rust-async-runtime-tokio".to_string());
    let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
    let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
    assert!(!names.contains(&"rust-async-runtime-tokio"));
}

#[test]
fn test_generate_patterns_unknown_deps() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n\n[dependencies]\nsome-obscure-crate = \"1\"\n",
    )
    .unwrap();
    let existing = HashSet::new();
    let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
    assert!(patterns.is_empty());
}

#[test]
fn test_generate_patterns_no_project() {
    let dir = tempfile::tempdir().unwrap();
    let existing = HashSet::new();
    let patterns = generate_starter_patterns(dir.path(), &existing).unwrap();
    assert!(patterns.is_empty());
}

#[test]
fn test_project_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("myproject");
    fs::create_dir_all(&project_path).unwrap();

    // Point the tracking store at a temp root. Without this the test
    // writes into the real ~/.mur/projects — which pollutes the user's
    // home on a good day and fails with EPERM under the sandbox.
    // SAFETY: nextest runs each test in its own process.
    let mur_home = tempfile::tempdir().unwrap();
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", mur_home.path());

    // Not known yet
    assert!(!is_known_project(&project_path).unwrap());

    // Mark it
    let info = ProjectInfo {
        path: project_path.to_string_lossy().to_string(),
        language: Language::Rust,
        deps: vec!["tokio".to_string()],
        generated_at: "2026-03-06T00:00:00Z".to_string(),
        patterns_generated: vec!["rust-async-runtime-tokio".to_string()],
    };

    mark_project_known(&project_path, info).unwrap();
    assert!(is_known_project(&project_path).unwrap());

    // And it landed under MUR_HOME, not the real home dir.
    assert!(
        mur_home.path().join("projects").exists(),
        "project info should be written under $MUR_HOME"
    );
}

#[test]
fn test_slugify() {
    assert_eq!(slugify("SwiftUI"), "swiftui");
    assert_eq!(slugify("laravel/framework"), "laravel-framework");
    assert_eq!(slugify("@scope/package"), "scope-package");
    assert_eq!(slugify("hello world"), "hello-world");
}

#[test]
fn test_pattern_fields() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n\n[dependencies]\ntokio = \"1\"\n",
    )
    .unwrap();
    let patterns = generate_starter_patterns(dir.path(), &HashSet::new()).unwrap();
    let p = &patterns[0];
    assert_eq!(p.tier, Tier::Project);
    assert_eq!(p.maturity, Maturity::Draft);
    assert!((p.confidence - 0.5).abs() < 0.001);
    assert!(p.tags.languages.contains(&"rust".to_string()));
    assert!(p.tags.topics.contains(&"starter".to_string()));
    assert_eq!(p.origin.as_ref().unwrap().source, "starter");
    assert_eq!(p.origin.as_ref().unwrap().trigger, OriginTrigger::Automatic);
}

#[test]
fn test_extract_composer_deps() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
            dir.path().join("composer.json"),
            r#"{"require":{"php":"^8.1","laravel/framework":"^10"},"require-dev":{"phpunit/phpunit":"^10"}}"#,
        ).unwrap();
    let deps = extract_composer_deps(dir.path());
    assert!(deps.contains(&"laravel/framework".to_string()));
    assert!(deps.contains(&"phpunit/phpunit".to_string()));
    assert!(!deps.contains(&"php".to_string())); // php itself is excluded
}

#[test]
fn test_extract_swift_deps() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Package.swift"),
        r#"
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "MyApp",
    dependencies: [
        .package(url: "https://github.com/apple/swift-testing.git", from: "0.1.0"),
    ],
    targets: [
        .target(name: "MyApp", dependencies: ["SwiftUI"]),
    ]
)
"#,
    )
    .unwrap();
    let deps = extract_swift_deps(dir.path());
    assert!(deps.contains(&"swift-testing".to_string()));
    assert!(deps.contains(&"SwiftUI".to_string()));
}
