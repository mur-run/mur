use mur_common::parallel::PreFilterKind;
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub enum FilterResult {
    Passed,
    Failed {
        filter: PreFilterKind,
        stderr: String,
    },
}

pub fn run_pre_filter(track_path: &Path, filters: &[PreFilterKind]) -> FilterResult {
    for filter in filters {
        let result = match filter {
            PreFilterKind::CargoCheck => run_cargo_check(track_path),
            PreFilterKind::CargoClippyDeny => run_clippy(track_path),
        };
        if let Err(stderr) = result {
            return FilterResult::Failed {
                filter: filter.clone(),
                stderr,
            };
        }
    }
    FilterResult::Passed
}

fn run_cargo_check(path: &Path) -> Result<(), String> {
    let out = Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(path)
        .env("ORT_STRATEGY", "download")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

fn run_clippy(path: &Path) -> Result<(), String> {
    let out = Command::new("cargo")
        .args(["clippy", "--quiet", "--", "-D", "warnings"])
        .current_dir(path)
        .env("ORT_STRATEGY", "download")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::PreFilterKind;

    #[test]
    fn non_existent_path_fails_gracefully() {
        let result = run_pre_filter(
            std::path::Path::new("/tmp/definitely_does_not_exist_parallel_test"),
            &[PreFilterKind::CargoCheck],
        );
        assert!(matches!(result, FilterResult::Failed { .. }));
    }
}
