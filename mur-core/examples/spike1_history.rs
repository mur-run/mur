//! Spike-1 (observational): measure the REAL N-way line-overlap rate from git
//! history, reusing the production classifier (`count_groups` → `group_edits`).
//!
//! Every genuine 2-parent merge is a natural concurrent-edit experiment: two
//! branches diverged from a common base and both edited files. We replay each
//! side vs the merge-base through the same hunk grouping `mur fleet
//! merge-concurrent --stats` uses, and aggregate clean vs overlapping groups.
//!
//! Run:  cargo run --release --example spike1_history -- [repo_path]
//!
//! Inference asymmetry: 2-parent merges are N=2. The CRDT's unique niche is
//! N>2. If even 2-way overlap is rare here, 3-way overlap is necessarily rarer
//! → STOP-on-Loro is supported a fortiori. (High 2-way overlap would only be
//! INVESTIGATE — it still wouldn't prove the N>2 case.)

use std::process::Command;

use mur_core::parallel::concurrent::stats::count_groups;
use mur_core::parallel::concurrent::structural::StructuralMerger;

fn git(repo: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

fn git_str(repo: &str, args: &[&str]) -> Option<String> {
    git(repo, args).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

fn main() {
    let repo = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let merger = StructuralMerger;

    let merges = git_str(&repo, &["log", "--merges", "--format=%H"]).unwrap_or_default();
    let merge_shas: Vec<&str> = merges.lines().collect();

    let mut divergent = 0usize;
    let mut files_compared = 0usize;
    let mut clean_groups = 0usize;
    let mut overlap_regions = 0usize;
    let mut merges_with_overlap = 0usize;
    let mut skipped_nonutf8 = 0usize;
    let mut overlap_examples: Vec<String> = Vec::new();

    for m in &merge_shas {
        // parents of this merge
        let line = match git_str(&repo, &["rev-list", "--parents", "-n", "1", m]) {
            Some(s) => s,
            None => continue,
        };
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            continue; // only clean 2-parent merges (parts = [merge, p1, p2])
        }
        let (p1, p2) = (parts[1], parts[2]);
        let base = match git_str(&repo, &["merge-base", p1, p2]) {
            Some(b) if !b.is_empty() => b,
            _ => continue,
        };
        // Skip catch-up merges (one side has no divergence) — not a real
        // "two branches both did work" experiment.
        if base == p1 || base == p2 {
            continue;
        }

        // Union of .rs files changed on either side vs base.
        let mut changed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for side in [p1, p2] {
            if let Some(out) = git_str(&repo, &["diff", "--name-only", &base, side]) {
                for l in out.lines() {
                    if l.ends_with(".rs") {
                        changed.insert(l.to_string());
                    }
                }
            }
        }
        if changed.is_empty() {
            continue;
        }
        divergent += 1;
        let mut this_merge_overlaps = 0usize;

        for rel in &changed {
            let base_bytes = git(&repo, &["show", &format!("{base}:{rel}")]).unwrap_or_default();
            let read_side = |side: &str| -> Vec<u8> {
                git(&repo, &["show", &format!("{side}:{rel}")])
                    .unwrap_or_else(|| base_bytes.clone())
            };
            let versions: Vec<(String, Vec<u8>)> = vec![
                ("p1".to_string(), read_side(p1)),
                ("p2".to_string(), read_side(p2)),
            ];

            match count_groups(&merger, &base_bytes, &versions) {
                Ok((c, o)) => {
                    files_compared += 1;
                    clean_groups += c;
                    overlap_regions += o;
                    this_merge_overlaps += o;
                    if o > 0 && overlap_examples.len() < 15 {
                        overlap_examples.push(format!("{} {rel}: {o} overlap(s)", &m[..8]));
                    }
                }
                Err(_) => skipped_nonutf8 += 1,
            }
        }
        if this_merge_overlaps > 0 {
            merges_with_overlap += 1;
        }
    }

    let total = clean_groups + overlap_regions;
    let rate = if total > 0 {
        overlap_regions as f64 / total as f64
    } else {
        0.0
    };

    println!("=== Spike-1 observational (git history, N=2 merges) ===");
    println!("repo:                  {repo}");
    println!("merge commits seen:    {}", merge_shas.len());
    println!("divergent merges:      {divergent}  (both sides edited .rs vs base)");
    println!("merges with ≥1 overlap: {merges_with_overlap}");
    println!("files compared:        {files_compared}");
    println!("clean groups:          {clean_groups}");
    println!("overlap regions:       {overlap_regions}");
    println!("skipped (non-utf8):    {skipped_nonutf8}");
    println!("OVERLAP RATE:          {:.1}%", rate * 100.0);
    println!();
    let gate = if total == 0 {
        "NO DATA"
    } else if rate < 0.05 {
        "STOP — StructuralMerger sufficient; skip Loro"
    } else if rate <= 0.20 {
        "INVESTIGATE (and note: N=2 only; N>2 rarer still)"
    } else {
        "INVESTIGATE→ (high 2-way; N>2 case still unproven)"
    };
    println!("gate (per spike1 doc): {gate}");
    if !overlap_examples.is_empty() {
        println!("\noverlap examples:");
        for e in &overlap_examples {
            println!("  {e}");
        }
    }
}
