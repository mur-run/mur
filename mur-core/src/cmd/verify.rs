use anyhow::Result;
use std::path::PathBuf;

use crate::verify::{self, ClaimKind, VerifyResult, VerifySummary};

/// `mur verify` — scan documentation for stale claims.
pub(crate) fn cmd_verify(file: Option<&str>, show_all: bool) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let results = if let Some(path) = file {
        let p = PathBuf::from(path);
        let abs = if p.is_absolute() {
            p
        } else {
            project_root.join(&p)
        };
        if !abs.exists() {
            anyhow::bail!("file not found: {}", abs.display());
        }
        verify::verify_file(&abs, &project_root)?
    } else {
        verify::verify_docs(&project_root)?
    };

    if results.is_empty() {
        println!("No verifiable claims found.");
        return Ok(());
    }

    let summary = VerifySummary::from_results(&results);

    // Group by source file
    let mut current_source = String::new();
    for r in &results {
        // Skip valid/skipped unless --all
        if !show_all && !matches!(r.result, VerifyResult::Invalid(_)) {
            continue;
        }

        if r.claim.source_file != current_source {
            current_source = r.claim.source_file.clone();
            println!("\n📄 {}", current_source);
            println!("{}", "─".repeat(current_source.len() + 3));
        }

        let icon = match &r.result {
            VerifyResult::Valid => "✅",
            VerifyResult::Invalid(_) => "❌",
            VerifyResult::Skipped(_) => "⏭️ ",
        };

        let kind_label = match &r.claim.kind {
            ClaimKind::FilePath(_) => "path",
            ClaimKind::Command(_) => "cmd",
            ClaimKind::CodeRef(_) => "code",
        };

        print!(
            "  {} L{:<4} [{}] {}",
            icon, r.claim.line_number, kind_label, r.claim.raw
        );

        match &r.result {
            VerifyResult::Invalid(reason) => println!("  ← {reason}"),
            VerifyResult::Skipped(reason) if show_all => println!("  ({reason})"),
            _ => println!(),
        }
    }

    // Summary
    println!();
    println!("━━━ Verify Summary ━━━");
    println!(
        "  Total: {}  ✅ Valid: {}  ❌ Invalid: {}  ⏭️  Skipped: {}",
        summary.total, summary.valid, summary.invalid, summary.skipped
    );

    if summary.invalid > 0 {
        println!(
            "\n  {} stale claim(s) found. Review and update documentation.",
            summary.invalid
        );
        // Exit non-zero so `mur verify` is usable as a CI / pre-commit gate.
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::process::exit(1);
    } else {
        println!("\n  All claims verified. Documentation is up to date.");
    }

    Ok(())
}
