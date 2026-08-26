//! Guard: the release archives must ship every binary the CLI expects to find.
//!
//! `release.yml` runs only on a tag, so PR CI never exercises the packaging
//! path — which is why two separate omissions reached users. #1027 signed one
//! binary of four, and `mur-research-gateway` was named by
//! `update::SIBLING_BINARIES` and `update::resign::SIGN_TARGETS` for months
//! while no packaging step shipped it, so `mur update` warned about a binary
//! the archive never contained and `mur deep-research` could not provision
//! workers on any non-developer install.
//!
//! Checking the archive against the workflow's own `BINARIES` list would be
//! circular — that list being incomplete IS the bug. So this asserts against
//! the independent consumer: whatever the CLI expects to re-sign after an
//! upgrade must be in the archive that upgrade reads from.
//!
//! Superset, not equality: shipping a binary nothing re-signs is harmless,
//! shipping one the CLI reaches for is the failure this prevents.

use std::path::PathBuf;

/// Workflow that builds and packages every release artifact, relative to this
/// crate's manifest directory.
const RELEASE_WORKFLOW: &str = "../.github/workflows/release.yml";

/// Job-level env key in that workflow declaring the shipped binary set.
const BINARIES_KEY: &str = "BINARIES:";

#[test]
fn release_archive_ships_every_binary_the_cli_expects() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(RELEASE_WORKFLOW);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    let line = yaml
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(BINARIES_KEY))
        .unwrap_or_else(|| {
            panic!(
                "no `{BINARIES_KEY}` declaration in {} — the packaging steps no \
                 longer share one list, so this guard cannot see what ships",
                path.display()
            )
        });

    let shipped: Vec<&str> = line[BINARIES_KEY.len()..].split_whitespace().collect();
    assert!(
        !shipped.is_empty(),
        "`{BINARIES_KEY}` in {} is empty",
        path.display()
    );

    let missing: Vec<&str> = mur_core::update::resign::SIGN_TARGETS
        .iter()
        .copied()
        .filter(|want| !shipped.contains(want))
        .collect();

    assert!(
        missing.is_empty(),
        "release.yml ships {shipped:?} but the CLI expects {missing:?} to be in \
         the archive (update::resign::SIGN_TARGETS). Add them to the job-level \
         `BINARIES` in {} — the sign loop, the tarball and the Windows zip all \
         read it. A binary missing here installs once via build.sh and then \
         never updates.",
        path.display()
    );
}
