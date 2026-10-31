use super::*;

#[test]
fn reveal_target_absolute_is_used_as_is() {
    let home = Path::new("/Users/x/.mur");
    assert_eq!(
        resolve_reveal_target("/abs/p", Some("a"), home),
        PathBuf::from("/abs/p")
    );
}

#[test]
fn reveal_target_relative_resolves_under_agent() {
    let home = Path::new("/Users/x/.mur");
    assert_eq!(
        resolve_reveal_target("skills/ctx", Some("rustsmith"), home),
        PathBuf::from("/Users/x/.mur/agents/rustsmith/skills/ctx")
    );
}

#[test]
fn reveal_target_relative_without_agent_under_home() {
    let home = Path::new("/Users/x/.mur");
    assert_eq!(
        resolve_reveal_target("models.yaml", None, home),
        PathBuf::from("/Users/x/.mur/models.yaml")
    );
}

fn stdio_entry(name: &str) -> mur_common::agent::McpServerEntry {
    mur_common::agent::McpServerEntry {
        name: name.to_string(),
        command: "npx".to_string(),
        args: vec!["-y".to_string(), name.to_string()],
        ..Default::default()
    }
}

fn remote_entry(name: &str, url: &str) -> mur_common::agent::McpServerEntry {
    mur_common::agent::McpServerEntry {
        name: name.to_string(),
        url: Some(url.to_string()),
        ..Default::default()
    }
}

#[test]
fn aggregate_shared_server_lists_both_agents_once() {
    let mut by_agent = std::collections::BTreeMap::new();
    by_agent.insert("a1".to_string(), vec![stdio_entry("shared")]);
    by_agent.insert("a2".to_string(), vec![stdio_entry("shared")]);

    let views = aggregate_mcp_servers(&by_agent);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].name, "shared");
    assert_eq!(views[0].transport, "stdio");
    assert_eq!(views[0].agents, vec!["a1".to_string(), "a2".to_string()]);
}

#[test]
fn aggregate_distinct_servers_produce_distinct_entries() {
    let mut by_agent = std::collections::BTreeMap::new();
    by_agent.insert(
        "a1".to_string(),
        vec![
            stdio_entry("local-one"),
            remote_entry("remote-one", "https://example.com/mcp"),
        ],
    );

    let views = aggregate_mcp_servers(&by_agent);
    assert_eq!(views.len(), 2);
    let remote = views.iter().find(|v| v.name == "remote-one").unwrap();
    assert_eq!(remote.transport, "remote");
    assert_eq!(remote.description, "https://example.com/mcp");
    assert_eq!(remote.agents, vec!["a1".to_string()]);
}

#[test]
fn aggregate_empty_map_returns_empty_list() {
    let by_agent = std::collections::BTreeMap::new();
    assert!(aggregate_mcp_servers(&by_agent).is_empty());
}

fn addon(id: &str, enabled: bool) -> mur_common::agent::AddonRef {
    mur_common::agent::AddonRef {
        id: id.to_string(),
        source: format!("claude-local:{id}@1.0.0"),
        enabled,
        skills: vec!["s1".to_string()],
        mcp: vec!["m1".to_string(), "m2".to_string()],
        commands: vec![],
        content_hash: None,
        fetch_ref: None,
        fetch_plugin: None,
    }
}

#[test]
fn aggregate_addons_shared_across_agents_one_entry_per_agent_state() {
    let mut by_agent = std::collections::BTreeMap::new();
    by_agent.insert("a1".to_string(), vec![addon("superpowers", true)]);
    by_agent.insert("a2".to_string(), vec![addon("superpowers", false)]);

    let views = aggregate_addons(&by_agent);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].id, "superpowers");
    assert_eq!(views[0].skill_count, 1);
    assert_eq!(views[0].mcp_count, 2);
    assert_eq!(views[0].command_count, 0);
    assert_eq!(
        views[0].agents,
        vec![
            AddonAgentState {
                agent: "a1".to_string(),
                enabled: true
            },
            AddonAgentState {
                agent: "a2".to_string(),
                enabled: false
            },
        ]
    );
}

#[test]
fn aggregate_addons_distinct_ids_produce_distinct_entries() {
    let mut by_agent = std::collections::BTreeMap::new();
    by_agent.insert(
        "a1".to_string(),
        vec![addon("superpowers", true), addon("quill", false)],
    );

    let views = aggregate_addons(&by_agent);
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].id, "quill");
    assert_eq!(views[1].id, "superpowers");
}

#[test]
fn aggregate_addons_empty_map_returns_empty_list() {
    let by_agent = std::collections::BTreeMap::new();
    assert!(aggregate_addons(&by_agent).is_empty());
}
