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
