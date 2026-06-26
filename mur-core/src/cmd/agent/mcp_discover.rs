//! Discover MCP servers already configured by *other* tools (Claude Desktop,
//! Claude Code, Cursor, VS Code, Windsurf, Antigravity, Codex, Gemini CLI) so
//! the user can selectively import them into a MUR agent — instead of only
//! being able to add a server by hand or from a local directory.
//!
//! The client registry is data (`default_clients()`), not hardcoded scan logic,
//! so supporting a new tool is one table row. JSON configs reuse the addon
//! importer's tolerant parser (`mcpServers`-wrapped *or* bare server map);
//! Codex stores its servers in a TOML `[mcp_servers.<name>]` table.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::addon::parse::parse_mcp_json;

/// One MCP server found in another tool's config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredServer {
    /// The tool this came from (e.g. `claude-desktop`).
    pub client: String,
    /// The server id within that tool's config.
    pub name: String,
    /// stdio launch command. Empty for remote (http/sse) servers.
    pub command: String,
    pub args: Vec<String>,
    /// The config file it was read from.
    pub source: PathBuf,
}

/// How a client stores its MCP server config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// JSON; servers under a top-level `mcpServers` key OR a bare server map.
    Json,
    /// TOML; servers under a `[mcp_servers.<name>]` table (Codex).
    Toml,
}

/// A known external tool and where it keeps its MCP config. Each entry in
/// `paths` is a literal file path that may start with `~` (expanded to the
/// user's home); every path that exists is scanned. Multiple paths cover tools
/// whose location varies by platform/install.
// ponytail: literal paths only — no current client needs a glob; add globbing
// (the `glob` crate) if a future tool stores configs under a wildcard dir.
pub struct ClientDef {
    pub client: &'static str,
    pub paths: &'static [&'static str],
    pub format: ConfigFormat,
}

/// Built-in registry of tools we know how to scan for MCP servers. Adding a
/// tool is one row here — no scan-logic changes.
pub fn default_clients() -> Vec<ClientDef> {
    vec![
        ClientDef {
            client: "claude-desktop",
            paths: &["~/Library/Application Support/Claude/claude_desktop_config.json"],
            format: ConfigFormat::Json,
        },
        ClientDef {
            client: "claude-code",
            paths: &["~/.claude.json"],
            format: ConfigFormat::Json,
        },
        ClientDef {
            client: "cursor",
            paths: &["~/.cursor/mcp.json"],
            format: ConfigFormat::Json,
        },
        ClientDef {
            client: "vscode",
            paths: &[
                "~/.vscode/mcp.json",
                "~/Library/Application Support/Code/User/mcp.json",
            ],
            format: ConfigFormat::Json,
        },
        ClientDef {
            client: "windsurf",
            paths: &["~/.codeium/windsurf/mcp_config.json"],
            format: ConfigFormat::Json,
        },
        ClientDef {
            // Path unconfirmed on this machine; multi-candidate glob — whichever
            // exists wins. Windsurf/Codeium lineage, so candidates mirror that.
            client: "antigravity",
            paths: &[
                "~/Library/Application Support/Antigravity/User/mcp_config.json",
                "~/.codeium/antigravity/mcp_config.json",
                "~/.antigravity/mcp_config.json",
            ],
            format: ConfigFormat::Json,
        },
        ClientDef {
            client: "gemini-cli",
            paths: &["~/.gemini/settings.json"],
            format: ConfigFormat::Json,
        },
        ClientDef {
            client: "codex",
            paths: &["~/.codex/config.toml"],
            format: ConfigFormat::Toml,
        },
    ]
}

/// Parse a single config file's contents into the servers it declares. Never
/// panics or errors: a malformed file yields an empty list (it is simply
/// skipped during discovery).
pub fn parse_client_config(
    client: &str,
    source: &Path,
    content: &str,
    format: ConfigFormat,
) -> Vec<DiscoveredServer> {
    match format {
        ConfigFormat::Json => match parse_mcp_json(content) {
            Ok(j) => j
                .mcp_servers
                .into_iter()
                .map(|(name, s)| DiscoveredServer {
                    client: client.to_string(),
                    name,
                    command: s.command,
                    args: s.args,
                    source: source.to_path_buf(),
                })
                .collect(),
            Err(_) => Vec::new(),
        },
        ConfigFormat::Toml => {
            let Ok(val) = content.parse::<toml::Value>() else {
                return Vec::new();
            };
            let Some(table) = val.get("mcp_servers").and_then(|v| v.as_table()) else {
                return Vec::new();
            };
            table
                .iter()
                .map(|(name, srv)| {
                    let command = srv
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let args = srv
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    DiscoveredServer {
                        client: client.to_string(),
                        name: name.clone(),
                        command,
                        args,
                        source: source.to_path_buf(),
                    }
                })
                .collect()
        }
    }
}

/// Collapse servers that are byte-for-byte the same launch (name + command +
/// args), keeping the first seen. The same server configured in three tools
/// shows up once.
pub fn dedup(servers: Vec<DiscoveredServer>) -> Vec<DiscoveredServer> {
    let mut seen = std::collections::HashSet::new();
    servers
        .into_iter()
        .filter(|s| seen.insert((s.name.clone(), s.command.clone(), s.args.clone())))
        .collect()
}

/// Scan every client's config paths under `home`, parse each existing file, and
/// return the deduped set of MCP servers found. Missing/unreadable files are
/// silently skipped — discovery is best-effort.
pub fn discover(clients: &[ClientDef], home: &Path) -> Vec<DiscoveredServer> {
    let mut all = Vec::new();
    for client in clients {
        for raw in client.paths {
            let path = expand_home(raw, home);
            if let Ok(content) = std::fs::read_to_string(&path) {
                all.extend(parse_client_config(
                    client.client,
                    &path,
                    &content,
                    client.format,
                ));
            }
        }
    }
    dedup(all)
}

/// Expand a leading `~` in `path` against `home`. Returns the path unchanged if
/// it does not start with `~`.
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else if path == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(path)
    }
}

/// CLI entrypoint: scan the known tools and print the MCP servers found, with a
/// hint on how to import one. (Selective import lives in the Hub UI; this lists.)
pub fn cmd_mcp_discover() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
    let found = discover(&default_clients(), &home);
    if found.is_empty() {
        println!("No MCP servers found in other tools' configs.");
        return Ok(());
    }
    println!(
        "Found {} MCP server(s) configured in other tools:\n",
        found.len()
    );
    for s in &found {
        let launch = if s.command.is_empty() {
            "(remote/http — add manually)".to_string()
        } else if s.args.is_empty() {
            s.command.clone()
        } else {
            format!("{} {}", s.command, s.args.join(" "))
        };
        println!("  [{}] {}", s.client, s.name);
        println!("      {launch}");
        println!("      from: {}", s.source.display());
    }
    println!("\nImport one:  mur agent mcp add <agent> <name> --command <cmd> [--arg <a> ...]");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> PathBuf {
        PathBuf::from("/tmp/config")
    }

    #[test]
    fn parses_wrapped_mcpservers_json() {
        let c = r#"{"mcpServers":{"weather":{"command":"weather-mcp","args":["--port","9"]}}}"#;
        let got = parse_client_config("claude-desktop", &src(), c, ConfigFormat::Json);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].client, "claude-desktop");
        assert_eq!(got[0].name, "weather");
        assert_eq!(got[0].command, "weather-mcp");
        assert_eq!(got[0].args, vec!["--port", "9"]);
        assert_eq!(got[0].source, src());
    }

    #[test]
    fn parses_bare_map_json() {
        let c = r#"{"context7":{"command":"npx","args":["-y","@upstash/context7-mcp"]}}"#;
        let got = parse_client_config("cursor", &src(), c, ConfigFormat::Json);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "context7");
        assert_eq!(got[0].command, "npx");
    }

    #[test]
    fn parses_codex_toml_table() {
        let c =
            "[mcp_servers.airtable]\ncommand = \"npx\"\nargs = [\"-y\", \"airtable-mcp-server\"]\n";
        let got = parse_client_config("codex", &src(), c, ConfigFormat::Toml);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "airtable");
        assert_eq!(got[0].command, "npx");
        assert_eq!(got[0].args, vec!["-y", "airtable-mcp-server"]);
    }

    #[test]
    fn malformed_config_yields_empty_not_panic() {
        assert!(parse_client_config("x", &src(), "{not json", ConfigFormat::Json).is_empty());
        assert!(
            parse_client_config("x", &src(), "not = toml = bad", ConfigFormat::Toml).is_empty()
        );
    }

    #[test]
    fn dedup_collapses_same_launch_across_clients() {
        let a = DiscoveredServer {
            client: "claude-desktop".into(),
            name: "ctx".into(),
            command: "npx".into(),
            args: vec!["pkg".into()],
            source: src(),
        };
        let mut b = a.clone();
        b.client = "cursor".into();
        let got = dedup(vec![a, b]);
        assert_eq!(got.len(), 1, "same name+command+args collapses to one");
    }

    #[test]
    fn dedup_keeps_different_args() {
        let a = DiscoveredServer {
            client: "a".into(),
            name: "x".into(),
            command: "npx".into(),
            args: vec!["one".into()],
            source: src(),
        };
        let mut b = a.clone();
        b.args = vec!["two".into()];
        assert_eq!(dedup(vec![a, b]).len(), 2);
    }

    #[test]
    fn discover_reads_existing_configs_and_skips_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".cursor")).unwrap();
        std::fs::write(
            home.join(".cursor/mcp.json"),
            r#"{"ctx":{"command":"npx","args":["pkg"]}}"#,
        )
        .unwrap();
        let clients = [
            ClientDef {
                client: "cursor",
                paths: &["~/.cursor/mcp.json"],
                format: ConfigFormat::Json,
            },
            ClientDef {
                client: "absent",
                paths: &["~/.nope/missing.json"],
                format: ConfigFormat::Json,
            },
        ];
        let got = discover(&clients, home);
        assert_eq!(got.len(), 1, "found cursor, skipped the missing one");
        assert_eq!(got[0].client, "cursor");
        assert_eq!(got[0].name, "ctx");
        assert_eq!(got[0].command, "npx");
    }

    #[test]
    fn expand_home_replaces_leading_tilde() {
        let home = PathBuf::from("/Users/alice");
        assert_eq!(
            expand_home("~/.cursor/mcp.json", &home),
            PathBuf::from("/Users/alice/.cursor/mcp.json")
        );
        assert_eq!(expand_home("/abs/path", &home), PathBuf::from("/abs/path"));
    }
}
