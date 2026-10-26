use super::*;

fn view(refusals: &[(&str, &str)], cold: &[&str]) -> ReloadView {
    ReloadView {
        refusals: refusals
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        cold: cold.iter().map(|s| s.to_string()).collect(),
    }
}

fn listed_with(names: &[&str], digest: u64) -> Listed {
    Listed {
        entry: McpServerEntry::default(),
        names: names.iter().map(|s| s.to_string()).collect(),
        digest,
    }
}

#[test]
fn added_and_removed_names_make_one_notice_with_exactly_those_names() {
    let before = listed_with(&["click", "goto"], 1);
    let after = listed_with(&["goto", "snapshot"], 2);
    let n = name_diff("browser", &before, &after).expect("names changed");
    assert_eq!(
        n,
        "MCP `browser` reloaded: added `snapshot`; removed `click`"
    );
}

#[test]
fn a_schema_only_change_is_not_announced() {
    let before = listed_with(&["goto"], 1);
    let after = listed_with(&["goto"], 2);
    assert!(name_diff("browser", &before, &after).is_none());
    assert!(name_diff("browser", &before, &before).is_none());
}

#[test]
fn standing_notices_cover_refusals_and_mcp_restarts_only() {
    let servers = vec!["browser".to_string(), "fetch".to_string()];
    let n = standing_notices(
        &view(
            &[("fetch", "pin mismatch")],
            &[
                "mcp_servers.browser.url",
                "mcp_servers.browser.network.mode",
                "sandbox",
            ],
        ),
        &servers,
        "bob",
    );
    assert_eq!(n.len(), 2, "{n:?}");
    assert!(
        n[0].contains("`fetch` reload refused: pin mismatch"),
        "{}",
        n[0]
    );
    assert!(
        n[1].contains("`browser` edit to url, network.mode"),
        "{}",
        n[1]
    );
    assert!(n[1].contains("mur agent restart bob"), "{}", n[1]);
    let set = standing_notices(&view(&[], &["mcp_servers"]), &servers, "bob");
    assert_eq!(set.len(), 1);
    assert!(set[0].contains("added or removed"), "{}", set[0]);
}

#[test]
fn wire_names_are_attributed_by_sanitized_prefix() {
    assert!(belongs_to("mcp__my_server__go", "my/server"));
    assert!(!belongs_to("mcp__my_server2__go", "my/server"));
    assert!(!belongs_to("bash", "my/server"));
}
