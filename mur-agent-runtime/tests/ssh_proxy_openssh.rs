//! ssh-agent proxy against a real OpenSSH client and server (#2213, spec
//! "Binding sign requests to the verified session-bind", Tests:
//! "Integration: real OpenSSH client").
//!
//! A throwaway unprivileged `sshd` on loopback, a real `ssh-agent` upstream
//! holding the user key, and the production proxy (`listener::bind` /
//! `serve`, `self_only_gate`) in between. `ssh` talks only to the proxy.
//!
//! What this proves with real bytes, not fixtures: the client's
//! `session-bind@openssh.com` verifies, the sign request it sends passes
//! every binding rule (2-5) and the policy, and reaches the approval gate
//! with the server's real host key — once for each userauth method the
//! parser accepts, `publickey` and `publickey-hostbound-v00@openssh.com`,
//! chosen with `PubkeyAuthentication=unbound|host-bound` and confirmed from
//! the client's own debug line, not assumed from the server version. Approved, the upstream's signature goes
//! back and the login succeeds; denied, nothing is signed and the login
//! fails `hitl_denied`. A run with the host key left out of policy must
//! refuse `host_not_allowed` without asking, so the host fingerprint is the
//! server's, not a constant.
//!
//! No network beyond 127.0.0.1. A missing OpenSSH binary skips locally and
//! fails under CI, like `ssh_agent_dest_constraint.rs`.
#![cfg(unix)]

#[path = "support/openssh_rig.rs"]
mod openssh_rig;
use openssh_rig::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_openssh_login_is_signed_only_when_approved() {
    for bin in ["ssh", "ssh-agent", "ssh-add", "ssh-keygen"] {
        if on_path(bin).is_none() {
            assert!(!in_ci(), "{bin} not on PATH under CI");
            eprintln!("SKIP real_openssh_login: {bin} not on PATH");
            return;
        }
    }
    let Some(sshd) = find_sshd() else {
        assert!(!in_ci(), "sshd not found under CI");
        eprintln!("SKIP real_openssh_login: sshd not found");
        return;
    };
    let rig = Rig::start(&sshd);
    let (key_fp, host_fp) = (fp_of(&rig.user_key), fp_of(&rig.host_key));

    // Allowed key, allowed host, approved through the production gate B:
    // every binding rule and the policy pass, one `tool/approval_needed`
    // goes out, the answer comes back by `hitl_id`, and the upstream's
    // signature logs in. Hostbound first: the request carries the server
    // host key, so rule 4's host-key half is exercised with real bytes.
    let (gate_b, prompts) = approving_gate_b();
    let (out, refusals) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        gate_b.clone(),
        Method::Hostbound,
        // Tags stay short: on macOS the tempdir alone eats ~70 of the 104
        // bytes a unix socket path may have (`listener::socket_path_fits`).
        "ok-hb",
    )
    .await;
    assert!(
        out.status.success(),
        "approved hostbound login failed; refusals {:?}\n{}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>(),
        explain(&out, &rig)
    );
    assert_signed_with(&out, Method::Hostbound, &rig);
    assert!(
        refusals.is_empty(),
        "an approved login recorded refusals: {:?}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>()
    );
    assert_eq!(prompts.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Same pair, plain `publickey` (no host key in the signed data): the
    // parser's other accepted shape passes rules 2-4 too. Inside the TTL
    // the approval is remembered, so no second prompt.
    let (out, refusals) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        gate_b,
        Method::Publickey,
        "ok-pk",
    )
    .await;
    assert!(
        out.status.success(),
        "plain publickey login failed; refusals {:?}\n{}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>(),
        explain(&out, &rig)
    );
    assert_signed_with(&out, Method::Publickey, &rig);
    assert!(
        refusals.is_empty(),
        "a plain publickey login recorded refusals: {:?}",
        refusals.iter().map(|r| &r.code).collect::<Vec<_>>()
    );
    assert_eq!(
        prompts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a remembered approval prompted again"
    );

    // The gate is asked about exactly the real key and the server's real
    // host key.
    let probe = Answering::new(GateOutcome::Allow { decision: None });
    let (out, _) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        probe.clone(),
        Method::Hostbound,
        "asked",
    )
    .await;
    assert!(out.status.success(), "{}", explain(&out, &rig));
    let asked = probe.asked();
    assert_eq!(asked.len(), 1, "one prompt per login: {asked:?}");
    assert_eq!(asked[0].key_fp, key_fp);
    assert_eq!(asked[0].host_key_fp, host_fp);

    // Same pair, denied: nothing is signed, the login fails, and the reason
    // is the person's answer, not a binding rule.
    let (out, refusals) = login_through_proxy(
        &rig,
        rig.entitlement(vec![host_fp.clone()]),
        Answering::new(GateOutcome::Deny),
        Method::Hostbound,
        "denied",
    )
    .await;
    assert!(
        !out.status.success(),
        "denied login succeeded\n{}",
        explain(&out, &rig)
    );
    let codes: Vec<&str> = refusals.iter().map(|r| r.code.as_str()).collect();
    for c in BINDING_CODES {
        assert!(
            !codes.contains(&c.as_str()),
            "binding rule refused real client bytes: {codes:?}\n{}",
            explain(&out, &rig)
        );
    }
    let last = refusals
        .first()
        .unwrap_or_else(|| panic!("no refusal recorded\n{}", explain(&out, &rig)));
    assert_eq!(
        last.code,
        RefusalCode::HitlDenied.as_str(),
        "{codes:?}\n{}",
        explain(&out, &rig)
    );
    assert_eq!(last.key_fp.as_deref(), Some(key_fp.as_str()));
    assert_eq!(last.host_key_fp.as_deref(), Some(host_fp.as_str()));

    // Same key, server's host key not listed: the bind carried the real
    // server key, so the lookup must miss.
    // The gate must never be asked: a policy refusal is not a prompt.
    let never = Answering::new(GateOutcome::Allow { decision: None });
    let (out, refusals) = login_through_proxy(
        &rig,
        rig.entitlement(Vec::new()),
        never.clone(),
        Method::Hostbound,
        "no-host",
    )
    .await;
    assert!(!out.status.success());
    let last = refusals
        .first()
        .unwrap_or_else(|| panic!("no refusal recorded\n{}", explain(&out, &rig)));
    assert_eq!(
        last.code,
        RefusalCode::HostNotAllowed.as_str(),
        "{}",
        explain(&out, &rig)
    );
    assert_eq!(last.host_key_fp.as_deref(), Some(host_fp.as_str()));
    assert!(
        never.asked().is_empty(),
        "host_not_allowed reached the gate"
    );
}
