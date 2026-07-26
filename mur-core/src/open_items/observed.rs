//! Observed open items — derived from state MUR already holds.
//!
//! Every collector here aggregates. 244 pending proposals is one line, not 244
//! lines: a panel that can bury its own contents has the same practical value
//! as an empty one.

use std::path::Path;

use chrono::Utc;
use mur_common::fleet::JobStatus;

use super::{ItemSource, OpenItem};

pub fn collect(mur_home: &Path) -> Vec<OpenItem> {
    let mut out = Vec::new();
    out.extend(harvest_proposals(mur_home));
    out.extend(fleet_work(mur_home));
    out
}

fn observed(title: String, next: Option<String>, origin: String) -> OpenItem {
    OpenItem {
        title,
        next,
        source: ItemSource::Observed,
        origin,
        at: Utc::now(),
    }
}

/// Sessions the harvest gate turned into proposals that nobody has reviewed.
fn harvest_proposals(mur_home: &Path) -> Option<OpenItem> {
    let dir = mur_home.join("inbox").join("workflow-proposals");
    let n = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("yaml"))
                .count()
        })
        .unwrap_or(0);
    (n > 0).then(|| {
        observed(
            format!(
                "{n} harvested workflow proposal{} awaiting review",
                plural(n)
            ),
            Some("mur session out".into()),
            "inbox".into(),
        )
    })
}

/// Per fleet: unfinished jobs, and whether the kill-switch is holding it down.
///
/// A stopped fleet with queued work is the case worth surfacing — it will
/// never drain on its own, and nothing else says so.
fn fleet_work(mur_home: &Path) -> Vec<OpenItem> {
    let fleets_dir = mur_home.join("fleets");
    let Ok(entries) = std::fs::read_dir(&fleets_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten().filter(|e| e.path().is_dir()) {
        let name = e.file_name().to_string_lossy().to_string();
        let stopped = e.path().join(".stopped").exists();
        let (queued, running) = count_jobs(&e.path().join("jobs"));

        if queued + running > 0 {
            let mut title = format!("fleet '{name}': ");
            if queued > 0 {
                title.push_str(&format!("{queued} queued"));
            }
            if running > 0 {
                if queued > 0 {
                    title.push_str(", ");
                }
                title.push_str(&format!("{running} running"));
            }
            title.push_str(&format!(" job{}", plural(queued + running)));
            if stopped {
                title.push_str(" — but the fleet is stopped, so nothing will drain it");
            }
            out.push(observed(
                title,
                Some(if stopped {
                    format!("mur fleet start {name}")
                } else {
                    format!("mur fleet jobs {name}")
                }),
                format!("fleet:{name}"),
            ));
        } else if stopped {
            out.push(observed(
                format!("fleet '{name}' is stopped by its kill-switch"),
                Some(format!("mur fleet start {name}")),
                format!("fleet:{name}"),
            ));
        }
    }
    out.sort_by(|a, b| a.origin.cmp(&b.origin));
    out
}

fn count_jobs(jobs_dir: &Path) -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir(jobs_dir) else {
        return (0, 0);
    };
    let mut queued = 0;
    let mut running = 0;
    for e in entries.flatten() {
        if e.path().extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Ok(job) = serde_yaml_ng::from_str::<mur_common::fleet::Job>(&body) else {
            continue;
        };
        match job.status {
            JobStatus::Queued => queued += 1,
            JobStatus::Running => running += 1,
            _ => {}
        }
    }
    (queued, running)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn job(id: &str, status: JobStatus) -> mur_common::fleet::Job {
        mur_common::fleet::Job {
            id: id.into(),
            text: "do a thing".into(),
            source: "test".into(),
            status,
            created_at: Utc::now().to_rfc3339(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        }
    }

    #[test]
    fn nothing_outstanding_yields_nothing() {
        assert!(collect(home().path()).is_empty());
    }

    /// 244 proposals must be one line. Itemising them would bury every other
    /// open item under a wall of filenames.
    #[test]
    fn proposals_are_counted_not_listed() {
        let h = home();
        let dir = h.path().join("inbox").join("workflow-proposals");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..244 {
            std::fs::write(dir.join(format!("s{i}.yaml")), "x").unwrap();
        }
        let items = collect(h.path());
        assert_eq!(items.len(), 1);
        assert!(items[0].title.contains("244"), "{}", items[0].title);
        assert_eq!(items[0].next.as_deref(), Some("mur session out"));
    }

    /// The case that has no other alarm: work is queued and the kill-switch
    /// means it will sit there forever.
    #[test]
    fn stopped_fleet_with_queued_work_says_it_will_not_drain() {
        let h = home();
        let f = h.path().join("fleets").join("acme");
        std::fs::create_dir_all(f.join("jobs")).unwrap();
        std::fs::write(f.join(".stopped"), "").unwrap();
        std::fs::write(
            f.join("jobs").join("j1.yaml"),
            serde_yaml_ng::to_string(&job("j1", JobStatus::Queued)).unwrap(),
        )
        .unwrap();

        let items = collect(h.path());
        assert_eq!(items.len(), 1);
        assert!(items[0].title.contains("1 queued"), "{}", items[0].title);
        assert!(items[0].title.contains("stopped"), "{}", items[0].title);
        assert_eq!(items[0].next.as_deref(), Some("mur fleet start acme"));
    }

    /// Terminal jobs are not open items — a fleet that finished its work
    /// should fall silent rather than keep advertising it.
    #[test]
    fn finished_jobs_are_not_open() {
        let h = home();
        let f = h.path().join("fleets").join("done");
        std::fs::create_dir_all(f.join("jobs")).unwrap();
        std::fs::write(
            f.join("jobs").join("j1.yaml"),
            serde_yaml_ng::to_string(&job("j1", JobStatus::Done)).unwrap(),
        )
        .unwrap();
        assert!(collect(h.path()).is_empty());
    }

    /// A stopped fleet with nothing queued is still worth one line: somebody
    /// hit the kill-switch and may not remember.
    #[test]
    fn stopped_but_idle_fleet_still_reports() {
        let h = home();
        let f = h.path().join("fleets").join("idle");
        std::fs::create_dir_all(&f).unwrap();
        std::fs::write(f.join(".stopped"), "").unwrap();
        let items = collect(h.path());
        assert_eq!(items.len(), 1);
        assert!(items[0].title.contains("kill-switch"), "{}", items[0].title);
    }
}
