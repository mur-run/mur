//! `mur compress` / `mur retrieve` — thin CLI over mur-compress.

use std::io::Read;

use mur_compress::{CompressConfig, CompressEngine, RetrieveResult};

fn engine() -> anyhow::Result<CompressEngine> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let cfg = CompressConfig::load(&home);
    Ok(CompressEngine::new(home.join("compress"), cfg)?)
}

fn read_input(file: Option<&str>) -> anyhow::Result<String> {
    match file {
        Some("-") | None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
        Some(path) => Ok(std::fs::read_to_string(path)?),
    }
}

pub fn do_compress(file: Option<&str>, query: Option<&str>) -> anyhow::Result<()> {
    let eng = engine()?;
    let content = read_input(file)?;
    let r = eng.compress(&content, query);
    println!("{}", r.compressed);
    eprintln!(
        "[{} -> {} tokens ({:.1}% saved){}]",
        r.original_tokens,
        r.compressed_tokens,
        r.savings_percent,
        r.hash.map(|h| format!(", hash={h}")).unwrap_or_default()
    );
    Ok(())
}

pub fn do_retrieve(hash: &str, query: Option<&str>) -> anyhow::Result<()> {
    let eng = engine()?;
    match eng.retrieve(hash, query) {
        RetrieveResult::Full {
            original_content, ..
        } => println!("{original_content}"),
        RetrieveResult::Filtered { results, .. } => {
            for r in results {
                println!("{r}");
            }
        }
        RetrieveResult::NotFound => {
            anyhow::bail!("content not found or expired for hash={hash}");
        }
    }
    Ok(())
}
