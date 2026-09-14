use anyhow::Result;

use super::ProjectStatusInfo;

/// Version of the `mur project status --json` payload. Bump ONLY on a
/// breaking field change. No consumer gates on this number today — the
/// only reader (`mur-core/src/skills/mur_project_search.yaml`) decides
/// index usability from `indexed` / `indexing_in_progress` / `stale_dims`
/// — it is emitted so a future breaking change is loud rather than silent.
pub const PROJECT_STATUS_SCHEMA_VERSION: u32 = 1;

/// Machine-readable envelope for `mur project status --json`.
/// `info` is flattened so the JSON keys stay identical to
/// `ProjectStatusInfo`'s — the version is additive, not a nesting change.
///
/// `info.last_indexed` is always `null` on this path: `do_project_status`
/// (in `cmd/project/mod.rs`) never fills it, only `do_project_list` does
/// for its own view. Do not read it as a freshness signal here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectStatusJson {
    pub schema_version: u32,
    #[serde(flatten)]
    pub info: ProjectStatusInfo,
}

/// Serialize one project's status as versioned, pretty-printed JSON.
// ponytail: clones a small struct instead of threading a lifetime through
// the wrapper; borrow it if this ever lands in a hot loop.
pub fn status_json(info: &ProjectStatusInfo) -> Result<String> {
    let payload = ProjectStatusJson {
        schema_version: PROJECT_STATUS_SCHEMA_VERSION,
        info: info.clone(),
    };
    Ok(serde_json::to_string_pretty(&payload)?)
}

#[cfg(test)]
mod status_json_tests {
    use super::super::IndexProgressInfo;
    use super::*;

    fn base() -> ProjectStatusInfo {
        ProjectStatusInfo {
            name: "mur".into(),
            path: "/tmp/mur".into(),
            indexed: false,
            chunks: None,
            last_indexed: None,
            indexing_in_progress: false,
            progress: None,
            stale_dims: None,
        }
    }

    fn parse(info: &ProjectStatusInfo) -> serde_json::Value {
        serde_json::from_str(&status_json(info).expect("serialize")).expect("valid JSON")
    }

    #[test]
    fn not_indexed_serializes_with_schema_version() {
        let v = parse(&base());
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["name"], "mur");
        assert_eq!(v["path"], "/tmp/mur");
        assert_eq!(v["indexed"], false);
        assert!(v["chunks"].is_null());
        assert_eq!(v["indexing_in_progress"], false);
        assert!(v["stale_dims"].is_null());
    }

    #[test]
    fn usable_index_reports_chunks_and_no_staleness() {
        let info = ProjectStatusInfo {
            indexed: true,
            chunks: Some(123),
            ..base()
        };
        let v = parse(&info);
        assert_eq!(v["indexed"], true);
        assert_eq!(v["chunks"], 123);
        assert_eq!(v["indexing_in_progress"], false);
        assert!(v["stale_dims"].is_null());
    }

    #[test]
    fn indexing_in_progress_carries_progress_object() {
        let info = ProjectStatusInfo {
            indexed: true,
            chunks: Some(10),
            indexing_in_progress: true,
            progress: Some(IndexProgressInfo {
                done_chunks: 5,
                total_chunks: 20,
                pct: 25.0,
                errors: 1,
            }),
            ..base()
        };
        let v = parse(&info);
        assert_eq!(v["indexing_in_progress"], true);
        assert_eq!(v["progress"]["done_chunks"], 5);
        assert_eq!(v["progress"]["total_chunks"], 20);
        assert_eq!(v["progress"]["errors"], 1);
    }

    #[test]
    fn stale_dims_serializes_as_recorded_then_configured() {
        let info = ProjectStatusInfo {
            indexed: true,
            chunks: Some(7),
            stale_dims: Some((768, 1024)),
            ..base()
        };
        let v = parse(&info);
        assert_eq!(v["stale_dims"][0], 768);
        assert_eq!(v["stale_dims"][1], 1024);
    }

    /// The JSON keys are a contract for skills and agents: a rename here
    /// silently breaks every consumer's `usable` check. Pin the whole key set.
    #[test]
    fn json_key_set_is_exactly_the_documented_contract() {
        let v = parse(&base());
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "chunks",
                "indexed",
                "indexing_in_progress",
                "last_indexed",
                "name",
                "path",
                "progress",
                "schema_version",
                "stale_dims",
            ]
        );
    }
}
