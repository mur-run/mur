//! Official MCP Registry client (`registry.modelcontextprotocol.io`).
//!
//! `mur agent mcp search <query>` lists matching servers; `mur agent mcp
//! registry-add <agent> <name>` resolves a server's stdio package (npm→npx,
//! pypi→uvx) into a launch command and installs it via the normal
//! `cmd_mcp_add` path (binary pin + approval prompt). Remote-only servers
//! (Streamable HTTP, no package) are installed by URL via `cmd_mcp_add_remote`;
//! run `mur agent mcp login <agent> <id>` afterward to add authentication.

use anyhow::Result;
use serde::Deserialize;

use super::mcp::{McpAddPin, cmd_mcp_add, cmd_mcp_add_remote};

/// Base URL of the official registry. Documented endpoint, not a tunable.
const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";

#[derive(Debug, Clone, Deserialize)]
struct ServersResponse {
    #[serde(default)]
    servers: Vec<ServerEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ServerEntry {
    server: RegistryServer,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryServer {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub packages: Vec<RegistryPackage>,
    #[serde(default)]
    pub remotes: Vec<RegistryRemote>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryPackage {
    #[serde(rename = "registryType", default)]
    pub registry_type: String,
    #[serde(default)]
    pub identifier: String,
    #[serde(rename = "runtimeHint", default)]
    pub runtime_hint: String,
    #[serde(rename = "runtimeArguments", default)]
    pub runtime_arguments: Vec<RuntimeArg>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeArg {
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryRemote {
    #[serde(rename = "type", default)]
    pub r#type: String,
    #[serde(default)]
    pub url: String,
}

/// Parse the `/v0/servers` response into the server list.
pub fn parse_servers(body: &str) -> Result<Vec<RegistryServer>> {
    let resp: ServersResponse = serde_json::from_str(body)?;
    Ok(resp.servers.into_iter().map(|e| e.server).collect())
}

/// Build the `(command, args)` to launch a registry package as a stdio MCP
/// server: npm→`npx`, pypi→`uvx`, prepending the package's runtime arguments
/// before its identifier. Returns `None` for transports we can't launch as a
/// local stdio command yet (e.g. docker/oci) or an empty identifier.
pub fn package_command(pkg: &RegistryPackage) -> Option<(String, Vec<String>)> {
    if pkg.identifier.is_empty() {
        return None;
    }
    let cmd = match pkg.runtime_hint.as_str() {
        "npx" => "npx",
        "uvx" => "uvx",
        _ if pkg.registry_type == "npm" => "npx",
        _ if pkg.registry_type == "pypi" => "uvx",
        _ => return None,
    };
    let mut args: Vec<String> = pkg
        .runtime_arguments
        .iter()
        .map(|a| a.value.clone())
        .filter(|v| !v.is_empty())
        .collect();
    args.push(pkg.identifier.clone());
    Some((cmd.to_string(), args))
}

/// Short local id from a registry server name: take the last `/`-segment and
/// replace `.` with `-` so the result is a valid MCP server name token.
///
/// Examples: `"com.example/fs"` → `"fs"`, `"ac.foo.bar/my.server"` → `"my-server"`,
/// `"plain"` → `"plain"`.
fn short_id(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).replace('.', "-")
}

/// `mur agent mcp search <query>` — list registry servers matching `query`.
pub async fn cmd_mcp_search(query: &str) -> Result<()> {
    let body = reqwest::Client::new()
        .get(format!("{REGISTRY_BASE}/v0/servers"))
        .query(&[("search", query), ("limit", "20")])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("registry request: {e}"))?
        .text()
        .await?;
    let servers = parse_servers(&body)?;
    if servers.is_empty() {
        println!("No MCP servers found for '{query}'.");
        return Ok(());
    }
    println!("MCP servers matching '{query}':\n");
    for s in &servers {
        if !s.packages.is_empty() {
            println!("  {} [stdio]", s.name);
        } else if let Some(r) = s.remotes.first() {
            println!("  {} [remote: {} {}]", s.name, r.r#type, r.url);
        } else {
            println!("  {} [?]", s.name);
        }
        if !s.description.is_empty() {
            println!("      {}", s.description);
        }
    }
    println!("\nInstall a stdio one:  mur agent mcp registry-add <agent> <name>");
    Ok(())
}

/// `mur agent mcp registry-add <agent> <name>` — resolve a registry server's
/// stdio package and install it onto `agent` via `cmd_mcp_add`.
pub async fn cmd_mcp_registry_add(agent: &str, server_name: &str) -> Result<()> {
    let body = reqwest::Client::new()
        .get(format!("{REGISTRY_BASE}/v0/servers"))
        .query(&[("search", server_name), ("limit", "50")])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("registry request: {e}"))?
        .text()
        .await?;
    let servers = parse_servers(&body)?;
    let srv = servers
        .iter()
        .find(|s| s.name == server_name)
        .ok_or_else(|| anyhow::anyhow!("server '{server_name}' not found in the registry"))?;

    let id = short_id(server_name);

    if let Some((command, args)) = srv.packages.iter().find_map(package_command) {
        // Stdio package — install via the normal cmd_mcp_add path.
        cmd_mcp_add(
            agent,
            &id,
            &command,
            &args,
            McpAddPin {
                publisher_name: Some("mcp-registry".to_string()),
                publisher_registry_id: Some(server_name.to_string()),
                ..Default::default()
            },
        )
    } else if let Some(remote) = srv.remotes.first() {
        // Remote-only server (Streamable HTTP) — install by URL; no bearer token yet.
        println!(
            "'{server_name}' is a remote MCP server; installing by URL.\n\
             To authenticate, run: mur agent mcp login {agent} {id}"
        );
        cmd_mcp_add_remote(agent, &id, &remote.url, None)
    } else {
        anyhow::bail!("'{server_name}' has no installable package or remote endpoint")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "servers": [
        {"server": {
          "name": "com.example/fs",
          "description": "files",
          "packages": [
            {"registryType":"npm","identifier":"fs-mcp","runtimeHint":"npx","runtimeArguments":[{"value":"-y"}]}
          ]
        }},
        {"server": {
          "name": "ac.x/remote",
          "remotes": [{"type":"streamable-http","url":"https://x/mcp"}]
        }}
      ]
    }"#;

    #[test]
    fn short_id_takes_last_segment() {
        assert_eq!(short_id("com.example/fs"), "fs");
        assert_eq!(short_id("ac.foo.bar/my.server"), "my-server");
        assert_eq!(short_id("plain"), "plain");
    }

    #[test]
    fn parse_servers_reads_packages_and_remotes() {
        let servers = parse_servers(SAMPLE).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "com.example/fs");
        assert_eq!(servers[0].packages[0].identifier, "fs-mcp");
        assert_eq!(servers[1].remotes[0].r#type, "streamable-http");
    }

    #[test]
    fn package_command_maps_npm_and_pypi() {
        let npm = RegistryPackage {
            registry_type: "npm".into(),
            identifier: "fs-mcp".into(),
            runtime_hint: "npx".into(),
            runtime_arguments: vec![RuntimeArg { value: "-y".into() }],
        };
        assert_eq!(
            package_command(&npm),
            Some(("npx".into(), vec!["-y".into(), "fs-mcp".into()]))
        );

        let pypi = RegistryPackage {
            registry_type: "pypi".into(),
            identifier: "mcp-server-git".into(),
            runtime_hint: "uvx".into(),
            runtime_arguments: vec![],
        };
        assert_eq!(
            package_command(&pypi),
            Some(("uvx".into(), vec!["mcp-server-git".into()]))
        );

        // OCI/docker not launchable as a plain stdio command yet.
        let oci = RegistryPackage {
            registry_type: "oci".into(),
            identifier: "ghcr.io/x".into(),
            runtime_hint: "docker".into(),
            runtime_arguments: vec![],
        };
        assert_eq!(package_command(&oci), None);
    }
}
