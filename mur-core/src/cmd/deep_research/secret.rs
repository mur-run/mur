//! `mur deep-research secret --<provider>` — store a search-provider API key.
//!
//! The gap this closes: the gateway has always been able to READ a key from
//! `research_gateway.brave_api_key_ref` (a `keychain:mur/brave` SecretRef),
//! and its own blocked-search message tells operators to configure exactly
//! that — but MUR shipped no command that ever WRITES one. The only paths in
//! were hand-editing `config.yaml` with the key in plaintext, or exporting an
//! env var that dies with the shell.
//!
//! So the key goes to the OS keychain and only a *reference* to it is written
//! to config.yaml. The reference is safe to commit, log and sync; the secret
//! is not in the file at all.
//!
//! The key is read from a TTY without echo, or from stdin when piped, and is
//! never accepted as a command-line argument — argv is visible to every
//! process on the machine via `ps` and lands in shell history.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::research_provider::{KEYCHAIN_SERVICE, SearchProvider};

use crate::sources::credentials::{CredentialStore, OsKeyring};

/// The top-level config.yaml block the gateway reads its settings from.
const GATEWAY_BLOCK: &str = "research_gateway:";

/// Indent for keys inside a top-level block. Two spaces is what every block
/// `save_config` writes uses, and what the gateway's own docs show.
const BLOCK_INDENT: &str = "  ";

/// What `cmd_secret` did, so the caller can report it without re-deriving it.
#[derive(Debug, PartialEq, Eq)]
pub enum SecretOutcome {
    /// Key stored in the keychain; config.yaml now points at it.
    Stored { provider: SearchProvider },
    /// Key removed from the keychain and the ref dropped from config.yaml.
    Cleared { provider: SearchProvider },
}

/// Read the key without echoing it. Falls back to a plain line read when
/// stdin is not a TTY, which is what makes `echo $KEY | mur deep-research
/// secret --tavily` work in a provisioning script.
fn read_key(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    provider: SearchProvider,
) -> Result<String> {
    if std::io::stdin().is_terminal() {
        write!(
            output,
            "Paste your {} API key (input hidden, get one at {}): ",
            provider.display_name(),
            provider.signup_url()
        )?;
        output.flush()?;
        let key = rpassword::read_password().context("could not read the key without echo")?;
        writeln!(output)?;
        return Ok(key.trim().to_string());
    }
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        bail!("no key on stdin — pipe one in, or run this on a terminal to be prompted");
    }
    Ok(line.trim().to_string())
}

/// Validate the shape of a pasted key. Deliberately weak: every provider can
/// change its prefix at any time, so this rejects only what CANNOT be a key —
/// an empty string, or something that is obviously a whole shell command the
/// user pasted by mistake.
fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("empty key — nothing stored");
    }
    if key.chars().any(char::is_whitespace) {
        bail!("that key contains whitespace — paste only the key itself");
    }
    Ok(())
}

/// Textual upsert of one `research_gateway.<key>: <value>` entry in
/// config.yaml, returning the new file contents.
///
/// Edited as TEXT for the same reason `limits_write::upsert_global_limits`
/// is: round-tripping through the typed `Config` drops top-level blocks
/// mur-core has no field for, and `research_gateway:` is precisely the block
/// that was measurably lost that way (#778). Comments, key order and
/// formatting elsewhere in the file survive byte-for-byte.
///
/// Creates the block when absent, replaces the key when present, and leaves
/// every sibling key alone.
fn upsert_gateway_key(text: &str, key: &str, value: &str) -> String {
    let entry = format!("{BLOCK_INDENT}{key}: {value}");
    let lines: Vec<&str> = text.lines().collect();

    let Some(block_start) = lines
        .iter()
        .position(|l| l.trim_end() == GATEWAY_BLOCK || l.starts_with(GATEWAY_BLOCK))
    else {
        // No block yet: append a fresh one, keeping everything above intact.
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(GATEWAY_BLOCK);
        out.push('\n');
        out.push_str(&entry);
        out.push('\n');
        return out;
    };

    // The block runs to the next line starting in column 0 that is not blank
    // or a comment — same boundary rule as `upsert_global_limits`.
    let mut block_end = block_start + 1;
    while block_end < lines.len() {
        let l = lines[block_end];
        let top_level =
            !l.is_empty() && !l.starts_with(' ') && !l.starts_with('\t') && !l.starts_with('#');
        if top_level {
            break;
        }
        block_end += 1;
    }

    let existing = (block_start + 1..block_end)
        .find(|&i| lines[i].trim_start().starts_with(&format!("{key}:")));

    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    match existing {
        Some(i) => out[i] = entry,
        None => {
            // Insert at the END of the block so an existing key's position,
            // and any trailing comment lines inside it, stay put.
            let mut insert_at = block_end;
            while insert_at > block_start + 1 && lines[insert_at - 1].trim().is_empty() {
                insert_at -= 1;
            }
            out.insert(insert_at, entry);
        }
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    joined
}

/// Remove a `research_gateway.<key>` entry, returning the new contents. Used
/// by `--clear` so a cleared provider leaves no dangling ref behind.
fn remove_gateway_key(text: &str, key: &str) -> String {
    let needle = format!("{key}:");
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !(l.starts_with(' ') && l.trim_start().starts_with(&needle)))
        .collect();
    let mut joined = kept.join("\n");
    if !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// Write `text` to `config_path` via a temp file + rename, so an interrupted
/// write cannot leave a truncated config behind.
fn write_config_atomically(config_path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = config_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("rename to {}", config_path.display()))?;
    Ok(())
}

/// Entry point for `mur deep-research secret`. Resolves the provider flags,
/// reads the key without echo, and reports what changed.
///
/// No provider flag means Brave: it is the documented default and the only
/// backend that predates the other three, so a bare `mur deep-research
/// secret` keeps doing what a user of the older docs expects.
pub fn run(
    config_path: &Path,
    brave: bool,
    tavily: bool,
    serp_api: bool,
    firecrawl: bool,
    clear: bool,
    list: bool,
) -> Result<()> {
    let store = OsKeyring;
    if list {
        for row in cmd_secret_list(&store, config_path)? {
            println!("{row}");
        }
        return Ok(());
    }

    let provider = if tavily {
        SearchProvider::Tavily
    } else if serp_api {
        SearchProvider::SerpApi
    } else if firecrawl {
        SearchProvider::Firecrawl
    } else {
        let _ = brave;
        SearchProvider::Brave
    };

    if clear {
        cmd_secret_clear(&store, config_path, provider)?;
        println!(
            "removed the {} key — search falls back to the next configured provider, \
             or to keyless DuckDuckGo",
            provider.display_name()
        );
        return Ok(());
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stderr();
    let key = read_key(&mut input, &mut output, provider)?;
    cmd_secret(&store, config_path, provider, &key)?;
    println!(
        "{} key stored in the OS keychain; {} now points at {}",
        provider.display_name(),
        config_path.display(),
        provider.keychain_ref()
    );
    println!("restart any running research workers for it to take effect");
    Ok(())
}

/// Which providers currently have a key, WITHOUT revealing any of them.
/// `mur deep-research secret --list` exists so a user can answer "is my key
/// actually installed?" — a question that previously had no answer short of
/// opening Keychain Access.
pub fn cmd_secret_list(store: &dyn CredentialStore, config_path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut rows = Vec::new();
    for p in SearchProvider::all() {
        let in_keychain = store
            .get(KEYCHAIN_SERVICE, p.keychain_account())
            .unwrap_or(None)
            .is_some();
        let ref_in_config = text.contains(&format!("{}:", p.config_key_ref()));
        // Both halves matter: a key with no ref is never read, and a ref with
        // no key makes the gateway warn on every startup. Naming the half
        // that is missing is the whole point of the listing.
        let status = match (in_keychain, ref_in_config) {
            (true, true) => "configured",
            (true, false) => "key stored but config.yaml has no reference — re-run without --clear",
            (false, true) => "config.yaml references a key that is not in the keychain",
            (false, false) => "not configured",
        };
        rows.push(format!("{:<10} {status}", p.slug()));
    }
    Ok(rows)
}

/// Store `key` for `provider` in the OS keychain and point config.yaml at it.
///
/// Both halves are required for the key to be usable: the keychain holds the
/// secret, and `research_gateway.<slug>_api_key_ref` is what tells the
/// gateway to go look there. Writing only one of them is a silent no-op from
/// the user's point of view.
pub fn cmd_secret(
    store: &dyn CredentialStore,
    config_path: &Path,
    provider: SearchProvider,
    key: &str,
) -> Result<SecretOutcome> {
    validate_key(key)?;
    store
        .set(KEYCHAIN_SERVICE, provider.keychain_account(), key)
        .with_context(|| {
            format!(
                "could not store the {} key in the OS keychain",
                provider.display_name()
            )
        })?;
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let updated = upsert_gateway_key(&text, &provider.config_key_ref(), &provider.keychain_ref());
    write_config_atomically(config_path, &updated)?;
    Ok(SecretOutcome::Stored { provider })
}

/// Remove `provider`'s key from the keychain and drop its ref from config.
pub fn cmd_secret_clear(
    store: &dyn CredentialStore,
    config_path: &Path,
    provider: SearchProvider,
) -> Result<SecretOutcome> {
    store
        .delete(KEYCHAIN_SERVICE, provider.keychain_account())
        .with_context(|| {
            format!(
                "could not remove the {} key from the OS keychain",
                provider.display_name()
            )
        })?;
    if let Ok(text) = std::fs::read_to_string(config_path) {
        let updated = remove_gateway_key(&text, &provider.config_key_ref());
        write_config_atomically(config_path, &updated)?;
    }
    Ok(SecretOutcome::Cleared { provider })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::credentials::InMemoryCreds;

    /// A scratch config.yaml path unique to one test.
    fn scratch_config(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mur_dr_secret_{}_{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir.join("config.yaml")
    }

    #[test]
    fn stores_key_under_the_account_the_gateway_reads() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("account");
        cmd_secret(&store, &cfg, SearchProvider::Tavily, "tvly-abc123").unwrap();
        // The gateway resolves `keychain:mur/tavily`, so the key MUST be
        // filed under exactly that service/account pair or it is unreachable.
        assert_eq!(
            store.get(KEYCHAIN_SERVICE, "tavily").unwrap().as_deref(),
            Some("tvly-abc123")
        );
        // …and config.yaml must point at it, or nothing ever looks.
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            text.contains("tavily_api_key_ref: keychain:mur/tavily"),
            "{text}"
        );
    }

    /// The secret itself must NEVER reach config.yaml — that file is synced,
    /// diffed and pasted into bug reports. Only the reference belongs there.
    #[test]
    fn the_key_itself_never_lands_in_config() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("no_plaintext");
        cmd_secret(&store, &cfg, SearchProvider::Brave, "bsa-supersecret").unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("bsa-supersecret"), "key leaked:\n{text}");
    }

    /// Round-tripping config.yaml through the typed `Config` drops blocks
    /// mur-core has no field for (#778). This writer edits text, so every
    /// foreign block and comment must survive untouched.
    #[test]
    fn foreign_blocks_and_comments_survive() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("foreign");
        std::fs::write(
            &cfg,
            "# my notes\nllm:\n  provider: anthropic\nfleet_run:\n  agents: [mur]\n",
        )
        .unwrap();
        cmd_secret(&store, &cfg, SearchProvider::SerpApi, "serp-123").unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# my notes"), "{text}");
        assert!(text.contains("provider: anthropic"), "{text}");
        assert!(text.contains("agents: [mur]"), "{text}");
        assert!(text.contains("serpapi_api_key_ref:"), "{text}");
    }

    /// Adding a second provider must not disturb the first one's ref.
    #[test]
    fn a_second_provider_joins_the_existing_block() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("second");
        cmd_secret(&store, &cfg, SearchProvider::Brave, "bsa-1").unwrap();
        cmd_secret(&store, &cfg, SearchProvider::Tavily, "tvly-2").unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            text.contains("brave_api_key_ref: keychain:mur/brave"),
            "{text}"
        );
        assert!(
            text.contains("tavily_api_key_ref: keychain:mur/tavily"),
            "{text}"
        );
        // One block, not two.
        assert_eq!(text.matches("research_gateway:").count(), 1, "{text}");
    }

    /// Re-running for the same provider rotates the key in place rather than
    /// appending a duplicate YAML key (which would be a parse hazard).
    #[test]
    fn rotating_a_key_replaces_the_ref_in_place() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("rotate");
        cmd_secret(&store, &cfg, SearchProvider::Brave, "bsa-old").unwrap();
        cmd_secret(&store, &cfg, SearchProvider::Brave, "bsa-new").unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(text.matches("brave_api_key_ref").count(), 1, "{text}");
        assert_eq!(
            store.get(KEYCHAIN_SERVICE, "brave").unwrap().as_deref(),
            Some("bsa-new")
        );
    }

    /// An existing `research_gateway:` block keeps its other settings.
    #[test]
    fn existing_gateway_settings_are_preserved() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("existing_block");
        std::fs::write(
            &cfg,
            "research_gateway:\n  search_limit: 15\n  render_engine: obscura\n",
        )
        .unwrap();
        cmd_secret(&store, &cfg, SearchProvider::Firecrawl, "fc-1").unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("search_limit: 15"), "{text}");
        assert!(text.contains("render_engine: obscura"), "{text}");
        assert!(text.contains("firecrawl_api_key_ref:"), "{text}");
    }

    #[test]
    fn clearing_removes_the_key_and_the_ref() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("clear");
        cmd_secret(&store, &cfg, SearchProvider::Brave, "bsa-xyz").unwrap();
        cmd_secret_clear(&store, &cfg, SearchProvider::Brave).unwrap();
        assert_eq!(store.get(KEYCHAIN_SERVICE, "brave").unwrap(), None);
        // A dangling ref would make the gateway warn on every startup.
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("brave_api_key_ref"), "{text}");
    }

    /// Clearing one provider must not take its neighbour's ref with it.
    #[test]
    fn clearing_one_provider_leaves_the_others() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("clear_one");
        cmd_secret(&store, &cfg, SearchProvider::Brave, "bsa-1").unwrap();
        cmd_secret(&store, &cfg, SearchProvider::Tavily, "tvly-2").unwrap();
        cmd_secret_clear(&store, &cfg, SearchProvider::Brave).unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("brave_api_key_ref"), "{text}");
        assert!(text.contains("tavily_api_key_ref"), "{text}");
        assert_eq!(
            store.get(KEYCHAIN_SERVICE, "tavily").unwrap().as_deref(),
            Some("tvly-2")
        );
    }

    /// Clearing a provider that was never configured is a no-op, not an
    /// error — the end state the user asked for is already true.
    #[test]
    fn clearing_an_unset_provider_is_not_an_error() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("clear_unset");
        assert!(cmd_secret_clear(&store, &cfg, SearchProvider::SerpApi).is_ok());
    }

    #[test]
    fn empty_key_is_refused_and_stores_nothing() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("empty");
        let err = cmd_secret(&store, &cfg, SearchProvider::Brave, "").unwrap_err();
        assert!(err.to_string().contains("empty key"), "{err}");
        assert_eq!(store.get(KEYCHAIN_SERVICE, "brave").unwrap(), None);
        // A refused key must not have written a ref to a secret that is not there.
        assert!(
            !cfg.exists()
                || !std::fs::read_to_string(&cfg)
                    .unwrap()
                    .contains("brave_api_key_ref")
        );
    }

    /// A pasted `export MUR_RESEARCH_BRAVE_KEY=abc` would otherwise be stored
    /// verbatim and fail every search with an unhelpful 401.
    #[test]
    fn key_with_whitespace_is_refused() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("whitespace");
        let err = cmd_secret(&store, &cfg, SearchProvider::Brave, "export KEY=abc").unwrap_err();
        assert!(err.to_string().contains("whitespace"), "{err}");
        assert_eq!(store.get(KEYCHAIN_SERVICE, "brave").unwrap(), None);
    }

    /// The one seam that matters, pinned end to end: the EXACT config text
    /// this writer produces must be what the gateway's loader parses a key
    /// out of. The two live in different crates (mur-core writes, the
    /// standalone gateway binary reads) and no test can link both, so the
    /// contract is pinned here as the literal block shape, and in the
    /// gateway's `every_provider_resolves_ref_and_plaintext` as the literal
    /// parse of that same shape. A drift in either direction stores a secret
    /// nothing ever reads.
    #[test]
    fn written_config_matches_the_shape_the_gateway_parses() {
        let store = InMemoryCreds::default();
        let cfg = scratch_config("seam");
        cmd_secret(&store, &cfg, SearchProvider::Tavily, "tvly-seam").unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();

        // 1. A top-level `research_gateway:` block — the only key the gateway
        //    looks under.
        assert!(text.starts_with("research_gateway:\n"), "{text}");
        // 2. The entry indented two spaces inside it, `<slug>_api_key_ref`,
        //    valued with a parseable SecretRef.
        assert!(
            text.contains("\n  tavily_api_key_ref: keychain:mur/tavily\n"),
            "{text}"
        );
        // 3. That value must parse as a SecretRef, or the gateway warns and
        //    falls through to a plaintext key that is not there.
        let parsed: mur_common::secret::SecretRef =
            SearchProvider::Tavily.keychain_ref().parse().unwrap();
        assert_eq!(
            parsed,
            mur_common::secret::SecretRef::Keychain {
                service: "mur".to_string(),
                account: "tavily".to_string(),
            }
        );
        // 4. And the account it names is where the key was actually filed.
        assert_eq!(
            store.get(KEYCHAIN_SERVICE, "tavily").unwrap().as_deref(),
            Some("tvly-seam")
        );
    }

    /// A keychain write that fails (locked keychain, denied entitlement, a
    /// sandbox) must not leave config.yaml claiming a key that is not there —
    /// that dangling ref makes the gateway warn on every startup and hides
    /// the real cause. Verified for real on 2026-07-13: under a sandbox that
    /// denies keychain writes, `mur deep-research secret --tavily` errored
    /// and config.yaml was left byte-identical.
    #[test]
    fn a_failed_keychain_write_leaves_config_untouched() {
        let store = FailingCreds;
        let cfg = scratch_config("keychain_fail");
        let before = "research_gateway:\n  search_limit: 15\n";
        std::fs::write(&cfg, before).unwrap();
        let err = cmd_secret(&store, &cfg, SearchProvider::Tavily, "tvly-x").unwrap_err();
        assert!(err.to_string().contains("keychain"), "{err}");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), before);
    }

    /// A store whose writes always fail, standing in for a locked or
    /// entitlement-denied keychain.
    struct FailingCreds;
    impl CredentialStore for FailingCreds {
        fn set(&self, _s: &str, _a: &str, _v: &str) -> Result<()> {
            anyhow::bail!("Platform secure storage failure: UNIX[Operation not permitted]")
        }
        fn get(&self, _s: &str, _a: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn delete(&self, _s: &str, _a: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reads_a_piped_key_from_stdin() {
        let mut input = std::io::Cursor::new(b"tvly-piped\n".to_vec());
        let mut out = Vec::new();
        // Only meaningful when stdin is not a TTY, which it is not under test.
        if !std::io::stdin().is_terminal() {
            let key = read_key(&mut input, &mut out, SearchProvider::Tavily).unwrap();
            assert_eq!(key, "tvly-piped");
        }
    }
}
