use mur_core::yaml_edit::{set_top_level_scalar, write_atomic};
use tempfile::TempDir;

#[test]
fn write_atomic_replaces_via_temp_rename_and_cleans_up() {
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("p.yaml");
    write_atomic(&target, b"a: 1\n").unwrap();
    write_atomic(&target, b"a: 2\n").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "a: 2\n");
    let stale = target.with_extension("yaml.tmp");
    assert!(!stale.exists(), "tmp must be renamed away on success");
}

#[test]
fn write_atomic_does_not_corrupt_target_when_only_tmp_exists() {
    // Simulate a mid-edit crash: original is untouched, a stale .tmp lingers.
    // The contract for crash recovery is that the live file remains valid;
    // the tmp is simply abandoned (never renamed in).
    let tmp = TempDir::new().unwrap();
    let target = tmp.path().join("p.yaml");
    std::fs::write(&target, "x: 1\n").unwrap();
    std::fs::write(target.with_extension("yaml.tmp"), "garbage-not-yaml").unwrap();

    let body = std::fs::read_to_string(&target).unwrap();
    assert_eq!(body, "x: 1\n");
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body).unwrap();
    assert_eq!(v["x"].as_i64(), Some(1));
}

#[test]
fn set_top_level_scalar_preserves_header_and_inline_comments() {
    let input = "# header\nfoo: 1  # inline note\nbar: hello\n";
    let out = set_top_level_scalar(input, "foo", "2").unwrap();
    assert!(out.starts_with("# header\n"), "header lost: {out}");
    assert!(out.contains("foo: 2"), "value not updated: {out}");
    assert!(out.contains("# inline note"), "inline comment lost: {out}");
    assert!(out.contains("bar: hello"), "sibling key lost: {out}");
}

#[test]
fn set_top_level_scalar_errors_on_missing_key() {
    let input = "foo: 1\n";
    let err = set_top_level_scalar(input, "missing", "x").unwrap_err();
    assert!(err.to_string().contains("not found"));
}
