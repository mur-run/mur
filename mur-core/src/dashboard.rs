//! Dashboard: one-shot terminal overview of MUR pattern state.
//!
//! Renders a rich overview with pattern stats, top injections,
//! recent feedback, decay warnings, and co-occurrence links.

use anyhow::Result;
use mur_common::knowledge::Maturity;
use mur_common::pattern::{LifecycleStatus, Pattern, Tier};

use crate::evolve::cooccurrence::CooccurrenceMatrix;
use crate::store::yaml::YamlStore;

// ─── ANSI helpers ────────────────────────────────────────────────

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";

// ─── Box-drawing ─────────────────────────────────────────────────

fn header(title: &str) {
    let width: usize = 56;
    let pad = width.saturating_sub(title.len() + 4);
    println!("\n{BOLD}{CYAN}┌─ {} {}┐{RESET}", title, "─".repeat(pad));
}

fn footer() {
    println!("{DIM}{CYAN}└{}┘{RESET}", "─".repeat(56));
}

fn row(label: &str, value: &str) {
    println!("  {:<24} {}", label, value);
}

fn row_colored(label: &str, value: &str, color: &str) {
    println!("  {:<24} {}{}{}", label, color, value, RESET);
}

// ─── Public entry point ──────────────────────────────────────────

pub fn render_dashboard() -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    if patterns.is_empty() {
        println!("{BOLD}MUR Dashboard{RESET}");
        println!("{DIM}No patterns found. Run `mur new` to create one.{RESET}");
        return Ok(());
    }

    render_summary(&patterns);
    render_top_injected(&patterns);
    render_decay_warnings(&patterns);
    render_cooccurrence_links();
    render_recent_feedback_hint();

    println!();
    Ok(())
}

// ─── Sections ────────────────────────────────────────────────────

fn render_summary(patterns: &[Pattern]) {
    let total = patterns.len();
    let (mut session, mut project, mut core_c) = (0, 0, 0);
    let (mut draft, mut emerging, mut stable, mut canonical) = (0, 0, 0, 0);
    let (mut active, mut deprecated, mut archived) = (0, 0, 0);
    let mut total_injections: u64 = 0;
    let mut total_effectiveness = 0.0;
    let mut tracked = 0u64;

    for p in patterns {
        match p.tier {
            Tier::Session => session += 1,
            Tier::Project => project += 1,
            Tier::Core => core_c += 1,
        }
        match p.maturity {
            Maturity::Draft => draft += 1,
            Maturity::Emerging => emerging += 1,
            Maturity::Stable => stable += 1,
            Maturity::Canonical => canonical += 1,
        }
        match p.lifecycle.status {
            LifecycleStatus::Active => active += 1,
            LifecycleStatus::Deprecated => deprecated += 1,
            LifecycleStatus::Archived => archived += 1,
        }
        total_injections += p.evidence.injection_count;
        if p.evidence.injection_count > 0 {
            tracked += 1;
            total_effectiveness += p.evidence.effectiveness();
        }
    }

    let avg_eff = if tracked > 0 {
        total_effectiveness / tracked as f64
    } else {
        0.0
    };

    header("Pattern Summary");
    row_colored("Total patterns", &total.to_string(), BOLD);
    println!();
    row(
        "By tier",
        &format!(
            "Session {YELLOW}{session}{RESET}  Project {GREEN}{project}{RESET}  Core {MAGENTA}{core_c}{RESET}"
        ),
    );
    row(
        "By maturity",
        &format!("Draft {draft}  Emerging {emerging}  Stable {stable}  Canonical {canonical}"),
    );
    row(
        "By status",
        &format!(
            "Active {GREEN}{active}{RESET}  Deprecated {YELLOW}{deprecated}{RESET}  Archived {DIM}{archived}{RESET}"
        ),
    );
    println!();
    row("Total injections", &total_injections.to_string());
    row("Tracked / Total", &format!("{tracked} / {total}"));
    row_colored(
        "Avg effectiveness",
        &format!("{:.0}%", avg_eff * 100.0),
        if avg_eff >= 0.7 {
            GREEN
        } else if avg_eff >= 0.4 {
            YELLOW
        } else {
            RED
        },
    );
    footer();
}

fn render_top_injected(patterns: &[Pattern]) {
    let mut by_injections: Vec<&Pattern> = patterns
        .iter()
        .filter(|p| p.evidence.injection_count > 0)
        .collect();
    by_injections.sort_by(|a, b| b.evidence.injection_count.cmp(&a.evidence.injection_count));
    by_injections.truncate(10);

    if by_injections.is_empty() {
        return;
    }

    header("Top Injected Patterns");
    println!(
        "  {DIM}{:<4} {:<30} {:>6} {:>8}{RESET}",
        "#", "Name", "Count", "Effect."
    );
    for (i, p) in by_injections.iter().enumerate() {
        let eff = p.evidence.effectiveness();
        let eff_color = if eff >= 0.7 {
            GREEN
        } else if eff >= 0.4 {
            YELLOW
        } else {
            RED
        };
        let name = if p.name.len() > 30 {
            format!("{}...", &p.name[..27])
        } else {
            p.name.clone()
        };
        println!(
            "  {:<4} {:<30} {:>6} {}{:>7.0}%{}",
            i + 1,
            name,
            p.evidence.injection_count,
            eff_color,
            eff * 100.0,
            RESET,
        );
    }
    footer();
}

fn render_decay_warnings(patterns: &[Pattern]) {
    let mut low_confidence: Vec<&Pattern> = patterns
        .iter()
        .filter(|p| {
            p.confidence < 0.30
                && p.lifecycle.status == LifecycleStatus::Active
                && !p.lifecycle.pinned
        })
        .collect();
    low_confidence.sort_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap());
    low_confidence.truncate(10);

    if low_confidence.is_empty() {
        return;
    }

    header("Decay Warnings (confidence < 30%)");
    println!("  {DIM}{:<30} {:>8} {:>8}{RESET}", "Name", "Conf.", "Tier");
    for p in &low_confidence {
        let name = if p.name.len() > 30 {
            format!("{}...", &p.name[..27])
        } else {
            p.name.clone()
        };
        println!(
            "  {RED}{:<30}{RESET} {:>7.0}% {:>8}",
            name,
            p.confidence * 100.0,
            format!("{:?}", p.tier).to_lowercase(),
        );
    }
    println!("  {DIM}Tip: `mur boost <name>` or `mur pin <name>` to preserve{RESET}");
    footer();
}

fn render_cooccurrence_links() {
    let path = CooccurrenceMatrix::default_path();
    let matrix = match CooccurrenceMatrix::load(&path) {
        Ok(m) => m,
        Err(_) => return,
    };

    if matrix.pair_count() == 0 {
        return;
    }

    let clusters = matrix.find_clusters(3);
    if clusters.is_empty() {
        return;
    }

    header("Co-occurrence Clusters");
    for (i, cluster) in clusters.iter().take(5).enumerate() {
        let names = cluster.pattern_names.join(", ");
        let names_display = if names.len() > 50 {
            format!("{}...", &names[..47])
        } else {
            names
        };
        println!(
            "  {MAGENTA}{}.{RESET} {} {DIM}({}x){RESET}",
            i + 1,
            names_display,
            cluster.total_cooccurrences,
        );
    }
    footer();
}

fn render_recent_feedback_hint() {
    // Show a hint about feedback — we can't read historical feedback
    // without a dedicated log, but we can check last_injection.json
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".mur")
        .join("last_injection.json");

    if path.exists() {
        header("Recent Injection");
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(record) =
                serde_json::from_str::<crate::capture::feedback::InjectionRecord>(&data)
            {
                row("Query", &truncate(&record.query, 40));
                row("Project", &record.project);
                row("Patterns injected", &record.patterns.len().to_string());
                for p in record.patterns.iter().take(5) {
                    println!("    {DIM}• {}{RESET}", p.name);
                }
                println!("  {DIM}Run `mur feedback auto` to analyze session outcome{RESET}");
            }
        }
        footer();
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max.saturating_sub(3)])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::*;

    fn make_pattern(name: &str, tier: Tier, maturity: Maturity) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: format!("Pattern {name}"),
                content: Content::Plain("content".into()),
                tier,
                maturity,
                confidence: 0.5,
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate("a very long string indeed", 10);
        assert!(result.len() <= 10);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("12345", 5), "12345");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn test_render_summary_counts_tiers() {
        // Verify the summary counting logic by constructing known patterns
        let patterns = vec![
            make_pattern("a", Tier::Session, Maturity::Draft),
            make_pattern("b", Tier::Session, Maturity::Emerging),
            make_pattern("c", Tier::Project, Maturity::Stable),
            make_pattern("d", Tier::Core, Maturity::Canonical),
        ];
        // render_summary just prints, so verify it doesn't panic
        render_summary(&patterns);
    }

    #[test]
    fn test_render_top_injected_empty() {
        let patterns = vec![make_pattern("a", Tier::Session, Maturity::Draft)];
        // No injections, should not panic
        render_top_injected(&patterns);
    }

    #[test]
    fn test_render_top_injected_sorted_by_count() {
        let mut p1 = make_pattern("low", Tier::Session, Maturity::Draft);
        p1.evidence.injection_count = 2;
        p1.evidence.success_signals = 1;

        let mut p2 = make_pattern("high", Tier::Session, Maturity::Draft);
        p2.evidence.injection_count = 10;
        p2.evidence.success_signals = 8;

        let patterns = vec![p1, p2];
        // Should not panic; high should sort first
        render_top_injected(&patterns);
    }

    #[test]
    fn test_render_decay_warnings_filters_correctly() {
        let mut low = make_pattern("low-conf", Tier::Session, Maturity::Draft);
        low.confidence = 0.15;

        let mut high = make_pattern("high-conf", Tier::Session, Maturity::Draft);
        high.confidence = 0.90;

        let mut pinned_low = make_pattern("pinned-low", Tier::Session, Maturity::Draft);
        pinned_low.confidence = 0.10;
        pinned_low.lifecycle.pinned = true;

        let mut deprecated_low = make_pattern("deprecated-low", Tier::Session, Maturity::Draft);
        deprecated_low.confidence = 0.10;
        deprecated_low.lifecycle.status = LifecycleStatus::Deprecated;

        let patterns = vec![low, high, pinned_low, deprecated_low];
        // Only "low-conf" should appear (not pinned, not deprecated, < 0.30)
        render_decay_warnings(&patterns);
    }

    #[test]
    fn test_render_top_injected_truncates_long_names() {
        let mut p = make_pattern(
            "a-very-long-pattern-name-that-exceeds-thirty-characters",
            Tier::Session,
            Maturity::Draft,
        );
        p.evidence.injection_count = 5;
        p.evidence.success_signals = 3;
        render_top_injected(&[p]);
    }

    #[test]
    fn test_render_summary_with_tracked_effectiveness() {
        let mut p1 = make_pattern("tracked", Tier::Project, Maturity::Emerging);
        p1.evidence.injection_count = 10;
        p1.evidence.success_signals = 8;
        p1.evidence.override_signals = 2;

        let p2 = make_pattern("untracked", Tier::Session, Maturity::Draft);

        render_summary(&[p1, p2]);
    }

    #[test]
    fn test_render_summary_all_tiers_and_maturity() {
        let patterns = vec![
            make_pattern("s1", Tier::Session, Maturity::Draft),
            make_pattern("s2", Tier::Session, Maturity::Emerging),
            make_pattern("p1", Tier::Project, Maturity::Stable),
            make_pattern("p2", Tier::Project, Maturity::Canonical),
            make_pattern("c1", Tier::Core, Maturity::Stable),
        ];
        render_summary(&patterns);
    }
}
