//! Guard: the canonical release manifest must ship every binary the CLI expects.
//!
//! Release workflows run only on tags, so PR CI needs an independent check
//! that packaging and updater expectations have not drifted apart.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;

const RELEASE_MANIFEST: &str = "../release/binaries.toml";

#[derive(Deserialize)]
struct Manifest {
    schema: u64,
    binary: Vec<Binary>,
}

#[derive(Deserialize)]
struct Binary {
    name: String,
}

#[test]
fn release_archive_matches_every_binary_the_cli_expects() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(RELEASE_MANIFEST);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let manifest: Manifest =
        toml::from_str(&source).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()));

    assert_eq!(manifest.schema, 1, "unsupported release manifest schema");

    let shipped: BTreeSet<&str> = manifest
        .binary
        .iter()
        .map(|binary| binary.name.as_str())
        .collect();
    assert_eq!(
        shipped.len(),
        manifest.binary.len(),
        "{} contains duplicate binary names",
        path.display()
    );

    let expected: BTreeSet<&str> = mur_core::update::resign::SIGN_TARGETS
        .iter()
        .copied()
        .chain(["mur"])
        .collect();

    assert_eq!(
        shipped,
        expected,
        "{} must exactly match update::resign::SIGN_TARGETS plus mur",
        path.display()
    );
}
