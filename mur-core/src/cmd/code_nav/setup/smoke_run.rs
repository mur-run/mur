//! What setup's closing smoke launch starts, fetching serena's own servers
//! first (#1981, #1993).

use std::io::Write;
use std::path::Path;

use anyhow::Result;

use super::super::consent::Manifest;
use super::super::plan::Lang;
use super::super::{serena_install, smoke, smoke_fetch};

/// Servers to launch, and the ones that could not be, with why.
pub(super) type SmokeTargets = (Vec<smoke::Target>, Vec<(Lang, String)>);

/// Why a server neither MUR nor setup's fetch provides goes unlaunched.
const NOT_MANAGED: &str = "serena starts it from a system install setup does not manage";

/// What setup's smoke launch starts for `langs`: MUR's own installs, plus the
/// servers serena fetches itself, fetched now into the entry's `serena_home`
/// (#1993). The second list is what could not be launched, and why.
pub(super) fn smoke_targets(
    manifest: &Manifest,
    langs: &[Lang],
    mur_home: &Path,
    serena_home: &Path,
    out: &mut dyn Write,
) -> Result<SmokeTargets> {
    let mut targets = smoke::targets(manifest, langs, mur_home);
    let rest: Vec<Lang> = langs
        .iter()
        .copied()
        .filter(|l| !targets.iter().any(|t| t.lang == *l))
        .collect();
    let to_fetch: Vec<Lang> = rest
        .iter()
        .copied()
        .filter(|l| l.serena_fetched())
        .collect();
    let mut unfetched: Vec<(Lang, String)> = rest
        .iter()
        .copied()
        .filter(|l| !l.serena_fetched())
        .map(|l| (l, NOT_MANAGED.into()))
        .collect();
    if !to_fetch.is_empty() {
        let names: Vec<_> = to_fetch.iter().map(|l| l.flag()).collect();
        writeln!(
            out,
            "\nFetching {} the way serena does, to smoke them…",
            names.join(", ")
        )?;
        let dir = serena_install::serena_dir(mur_home);
        for f in smoke_fetch::fetch(&dir, serena_home, &to_fetch) {
            match f {
                smoke_fetch::Fetched::Ready(t) => targets.push(t),
                smoke_fetch::Fetched::Failed { lang, detail } => unfetched.push((
                    lang,
                    format!("serena could not fetch it during setup: {detail}"),
                )),
            }
        }
    }
    Ok((targets, unfetched))
}
