use super::*;

fn mention(namespace: Option<Namespace>, name: &str) -> Mention {
    Mention {
        namespace,
        name: name.into(),
        ordinal: None,
        rest: String::new(),
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn unique_agent_resolves() {
    let got = resolve(
        &mention(None, "bob"),
        &names(&["alice", "bob"]),
        &names(&["ops"]),
    );
    assert_eq!(got, Ok(Resolved::Agent("bob".into())));
}

#[test]
fn unique_fleet_resolves() {
    let got = resolve(&mention(None, "ops"), &names(&["bob"]), &names(&["ops"]));
    assert_eq!(got, Ok(Resolved::Fleet("ops".into())));
}

#[test]
fn agent_and_fleet_same_name_is_ambiguous() {
    let got = resolve(&mention(None, "ops"), &names(&["ops"]), &names(&["ops"]));
    assert_eq!(got, Err(ResolveError::AmbiguousTarget("ops".into())));
    // A case-folded agent hit still counts as a match; no silent preference.
    let got = resolve(&mention(None, "ops"), &names(&["Ops"]), &names(&["ops"]));
    assert_eq!(got, Err(ResolveError::AmbiguousTarget("ops".into())));
}

#[test]
fn prefix_disambiguates() {
    let agents = names(&["ops"]);
    let fleets = names(&["ops"]);
    assert_eq!(
        resolve(&mention(Some(Namespace::Agent), "ops"), &agents, &fleets),
        Ok(Resolved::Agent("ops".into()))
    );
    assert_eq!(
        resolve(&mention(Some(Namespace::Fleet), "ops"), &agents, &fleets),
        Ok(Resolved::Fleet("ops".into()))
    );
}

#[test]
fn prefix_restricts_lookup_to_its_namespace() {
    assert_eq!(
        resolve(
            &mention(Some(Namespace::Fleet), "bob"),
            &names(&["bob"]),
            &[]
        ),
        Err(ResolveError::UnknownTarget("bob".into()))
    );
    assert_eq!(
        resolve(
            &mention(Some(Namespace::Agent), "ops"),
            &[],
            &names(&["ops"])
        ),
        Err(ResolveError::UnknownTarget("ops".into()))
    );
}

#[test]
fn unknown_is_unknown_target() {
    let got = resolve(&mention(None, "ghost"), &names(&["bob"]), &names(&["ops"]));
    assert_eq!(got, Err(ResolveError::UnknownTarget("ghost".into())));
}

#[test]
fn case_insensitive_agent_match() {
    let got = resolve(&mention(None, "BOB"), &names(&["Bob"]), &[]);
    assert_eq!(got, Ok(Resolved::Agent("Bob".into())));
}

#[test]
fn case_variants_with_no_exact_spelling_are_ambiguous() {
    let got = resolve(&mention(None, "bob"), &names(&["Bob", "BOB"]), &[]);
    assert_eq!(got, Err(ResolveError::AmbiguousTarget("bob".into())));
}

#[test]
fn fleet_match_is_exact_case() {
    let got = resolve(&mention(None, "OPS"), &[], &names(&["ops"]));
    assert_eq!(got, Err(ResolveError::UnknownTarget("OPS".into())));
}

#[test]
fn reserved_name_is_refused_even_if_constructed_directly() {
    let got = resolve(&mention(None, "all"), &names(&["all"]), &[]);
    assert_eq!(got, Err(ResolveError::ReservedName("all".into())));
}
