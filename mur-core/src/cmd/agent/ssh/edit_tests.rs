use super::*;

fn fp(c: char) -> String {
    format!("SHA256:{}", c.to_string().repeat(43))
}

fn kh(host: &str) -> SshKnownHost {
    SshKnownHost {
        host: host.into(),
        key_type: "ssh-ed25519".into(),
        key_base64: format!("B64{host}"),
    }
}

fn grant(key: char, host: Option<(char, Option<&str>)>) -> Grant {
    Grant {
        key_fp: fp(key),
        key_label: None,
        host: host.map(|(h, name)| HostGrant {
            host_key_fp: fp(h),
            label: name.map(str::to_string),
            known_host: name.map(kh),
        }),
    }
}

#[test]
fn allow_adds_then_refreshes_without_duplicating() {
    let mut e = SshEntitlement::default();
    assert_eq!(
        allow(&mut e, &grant('A', Some(('H', Some("gh"))))),
        AllowOutcome::Added
    );
    assert_eq!(
        allow(&mut e, &grant('A', Some(('H', Some("gh"))))),
        AllowOutcome::Updated
    );
    assert_eq!(e.keys.len(), 1);
    assert_eq!(e.keys[0].allow_hosts.len(), 1);
    assert_eq!(
        allow(&mut e, &grant('A', Some(('I', None)))),
        AllowOutcome::Added
    );
    assert_eq!(e.keys[0].allow_hosts.len(), 2);
}

#[test]
fn allow_keeps_a_stored_label_when_none_is_given() {
    let mut e = SshEntitlement::default();
    let mut g = grant('A', None);
    g.key_label = Some("deploy".into());
    allow(&mut e, &g);
    allow(&mut e, &grant('A', None));
    assert_eq!(e.keys[0].label.as_deref(), Some("deploy"));
}

fn sample() -> SshEntitlement {
    let mut e = SshEntitlement::default();
    for g in [
        grant('A', Some(('H', Some("h1")))),
        grant('A', Some(('I', Some("h2")))),
        grant('B', Some(('H', Some("h1")))),
        grant('C', None),
    ] {
        allow(&mut e, &g);
    }
    e
}

#[test]
fn revoke_pair_keeps_the_key_and_other_hosts() {
    let mut e = sample();
    let s = RevokeScope::new(Some(fp('A')), Some(fp('H'))).unwrap();
    assert!(!s.is_batch());
    assert_eq!(revoke(&mut e, &s).unwrap().len(), 1);
    assert_eq!(e.keys[0].allow_hosts.len(), 1);
    assert_eq!(e.keys[0].allow_hosts[0].host_key, fp('I'));
}

#[test]
fn revoke_key_drops_every_host_and_the_key() {
    let mut e = sample();
    let s = RevokeScope::new(Some(fp('A')), None).unwrap();
    assert!(s.is_batch());
    assert_eq!(revoke_preview(&e, &s).len(), 2);
    assert_eq!(e.keys.len(), 3, "preview changes nothing");
    assert_eq!(revoke(&mut e, &s).unwrap().len(), 2);
    assert!(e.keys.iter().all(|k| k.fingerprint != fp('A')));
}

#[test]
fn revoke_key_without_hosts_is_still_reported() {
    let mut e = sample();
    let r = revoke(&mut e, &RevokeScope::new(Some(fp('C')), None).unwrap()).unwrap();
    assert_eq!(
        r,
        vec![Removed {
            key_fp: fp('C'),
            host_key_fp: None
        }]
    );
}

#[test]
fn revoke_host_takes_it_from_every_key() {
    let mut e = sample();
    let s = RevokeScope::new(None, Some(fp('H'))).unwrap();
    assert!(s.is_batch());
    assert_eq!(revoke(&mut e, &s).unwrap().len(), 2);
    assert!(
        e.keys
            .iter()
            .flat_map(|k| &k.allow_hosts)
            .all(|h| h.host_key != fp('H'))
    );
}

#[test]
fn revoke_that_matches_nothing_fails_and_changes_nothing() {
    let mut e = sample();
    let before = e.clone();
    assert!(revoke(&mut e, &RevokeScope::new(Some(fp('Z')), None).unwrap()).is_err());
    assert_eq!(e, before);
    assert!(RevokeScope::new(None, None).is_err());
}

#[test]
fn known_hosts_lists_confirmed_entries_once_and_skips_fingerprint_only_rules() {
    let mut e = sample();
    allow(&mut e, &grant('D', Some(('J', None))));
    let out = render_known_hosts(&e);
    assert!(out.starts_with(KNOWN_HOSTS_HEADER));
    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(lines, vec!["h1 ssh-ed25519 B64h1", "h2 ssh-ed25519 B64h2"]);
}

#[test]
fn revoking_the_last_host_leaves_known_hosts_empty() {
    let mut e = SshEntitlement::default();
    allow(&mut e, &grant('A', Some(('H', Some("h1")))));
    assert!(!render_known_hosts(&e).is_empty());
    revoke(
        &mut e,
        &RevokeScope::new(Some(fp('A')), Some(fp('H'))).unwrap(),
    )
    .unwrap();
    assert_eq!(render_known_hosts(&e), "");
}
