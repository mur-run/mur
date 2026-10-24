//! `--read-username` / `--read-secret` (spec §5, §7.7, §7.8, §7.12): CLI refusals, the split
//! enrollment end to end, and the doctor's reading of it.
use super::*;
use std::os::unix::fs::PermissionsExt;

const TOKEN: &str = "ghp_write_token_value";
const READ_TOKEN: &str = "ghp_read_token_value!";

fn plant(f: &Fx, name: &str, body: &str, mode: u32) -> String {
    let dir = f.home.join(SECRETS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p.display().to_string()
}

fn split(write: &str, read: &str) -> EnrollAuth {
    EnrollAuth {
        username: Some("bot".into()),
        secret: Some(write.into()),
        read_username: Some("reader".into()),
        read_secret: Some(read.into()),
        allow_unauthenticated: false,
    }
}

/// Every file under the secrets directory with its contents, to prove enroll writes none.
fn secrets_snapshot(f: &Fx) -> Vec<(String, String)> {
    let dir = f.home.join(SECRETS_DIR);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<_> = rd
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    v.sort();
    v
}

#[derive(clap::Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    action: GitPushAction,
}

/// Spec §7.7, at the command line: the read flags need each other, need `--username`, and refuse
/// the anonymous opt-in.
#[test]
fn clap_refuses_a_lone_read_flag_a_read_flag_without_a_login_and_the_anonymous_opt_in() {
    use clap::Parser;
    let base = [
        "mur",
        "enroll",
        "--agent",
        "a",
        "--repo",
        "r",
        "--remote",
        "o",
        "--endpoint",
        HTTPS,
    ];
    let parse = |extra: &[&str]| Cli::try_parse_from(base.iter().chain(extra));
    let login = ["--username", "u", "--secret", "/s"];
    let read = ["--read-username", "ru", "--read-secret", "/rs"];
    assert!(parse(&[&login[..], &read[..]].concat()).is_ok());
    // Only one of the pair.
    assert!(parse(&[&login[..], &["--read-username", "ru"]].concat()).is_err());
    assert!(parse(&[&login[..], &["--read-secret", "/rs"]].concat()).is_err());
    // The pair without the write login.
    assert!(parse(&read).is_err());
    assert!(
        parse(&[
            "--username",
            "u",
            "--read-username",
            "ru",
            "--read-secret",
            "/rs"
        ])
        .is_err()
    );
    // The anonymous opt-in.
    assert!(parse(&[&["--allow-unauthenticated"][..], &read[..]].concat()).is_err());
}

/// Spec §7.7 for a caller that does not go through clap: the same refusals, in words that name
/// the fix, and nothing written.
#[test]
fn direct_callers_get_the_same_refusals_with_a_named_fix() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let r = plant(&f, "read", READ_TOKEN, 0o600);
    let cases: [(EnrollAuth, &str); 4] = [
        (
            EnrollAuth {
                read_username: None,
                ..split(&w, &r)
            },
            "--read-secret needs --read-username",
        ),
        (
            EnrollAuth {
                read_secret: None,
                ..split(&w, &r)
            },
            "--read-username needs --read-secret",
        ),
        (
            EnrollAuth {
                username: None,
                secret: None,
                ..split(&w, &r)
            },
            "--read-username and --read-secret need --username and --secret",
        ),
        (
            EnrollAuth {
                allow_unauthenticated: true,
                username: None,
                secret: None,
                ..split(&w, &r)
            },
            "--allow-unauthenticated",
        ),
    ];
    for (auth, needle) in cases {
        let (code, out) = go(&f, https_args(&f, auth));
        assert_eq!(code, 1, "{needle}: {out}");
        assert!(out.contains(needle), "{needle}: {out}");
        assert!(out.contains("nothing was written"), "{out}");
        assert_eq!(files(&f), (None, None));
    }
}

#[test]
fn read_flags_on_a_local_endpoint_are_refused() {
    let f = fx();
    let r = plant(&f, "read", READ_TOKEN, 0o600);
    let mut a = enroll_args(&f, "alice");
    if let GitPushAction::Enroll { auth, .. } = &mut a {
        *auth = EnrollAuth {
            read_username: Some("reader".into()),
            read_secret: Some(r),
            ..EnrollAuth::default()
        };
    }
    let (code, out) = go(&f, a);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("--read-username"), "{out}");
    assert!(out.contains("only apply to an https://"), "{out}");
    assert_eq!(files(&f), (None, None));
}

#[test]
fn a_malformed_read_username_or_path_is_refused_and_names_its_flag() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    for (user, path, needle) in [
        ("has space", "/abs/t", "--read-username"),
        ("u", "relative/t", "--read-secret"),
        ("u", "~/.mur/secrets/t", "--read-secret"),
    ] {
        let auth = EnrollAuth {
            read_username: Some(user.into()),
            read_secret: Some(path.into()),
            ..with_credential(w.clone())
        };
        let (code, out) = go(&f, https_args(&f, auth));
        assert_eq!(code, 1, "{out}");
        assert!(out.contains(needle), "{needle}: {out}");
        assert_eq!(files(&f), (None, None));
    }
}

#[test]
fn a_split_enrollment_is_stored_listed_and_never_writes_a_secret() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let r = plant(&f, "read", READ_TOKEN, 0o600);
    let before = secrets_snapshot(&f);
    let (code, out) = go(&f, https_args(&f, split(&w, &r)));
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        secrets_snapshot(&f),
        before,
        "enroll must not write a secret"
    );
    let rec = load(&f.home).unwrap().unwrap().enrollments[0].clone();
    assert_eq!(rec.credential.as_ref().unwrap().secret_path.as_str(), w);
    let read = rec.read_credential.expect("read credential stored");
    assert_eq!(read.username.as_str(), "reader");
    assert_eq!(read.secret_path.as_str(), r);

    let (_, json) = go(&f, GitPushAction::List { json: true });
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v[0]["read_credential"]["username"], "reader");
    assert_eq!(v[0]["read_credential"]["secret_path"], r.as_str());
    assert_eq!(v[0]["credential"]["username"], "bot");
    let (_, text) = go(&f, GitPushAction::List { json: false });
    assert!(text.contains("reads as reader"), "{text}");
    for shown in [&out, &json, &text] {
        assert!(
            !shown.contains(TOKEN) && !shown.contains(READ_TOKEN),
            "{shown}"
        );
    }
}

/// A single-credential listing still shows no `read_credential` key's value, as before.
#[test]
fn a_single_credential_listing_has_a_null_read_credential() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    go(&f, https_args(&f, with_credential(w)));
    let (_, json) = go(&f, GitPushAction::List { json: true });
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v[0]["read_credential"].is_null(), "{json}");
    let (_, text) = go(&f, GitPushAction::List { json: false });
    assert!(!text.contains("reads as"), "{text}");
}

/// Spec §7.4 from the CLI: adding then removing the read credential is an update each time.
#[test]
fn adding_and_removing_the_read_login_are_updates() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let r = plant(&f, "read", READ_TOKEN, 0o600);
    assert_eq!(go(&f, https_args(&f, with_credential(w.clone()))).0, 0);
    let e0 = load(&f.home).unwrap().unwrap().enrollments[0].epoch;
    let (code, out) = go(&f, https_args(&f, split(&w, &r)));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("updated"), "{out}");
    let e1 = load(&f.home).unwrap().unwrap().enrollments[0].epoch;
    assert!(e1 > e0);
    let (code, out) = go(&f, https_args(&f, with_credential(w)));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("updated"), "{out}");
    let rec = load(&f.home).unwrap().unwrap().enrollments[0].clone();
    assert!(rec.read_credential.is_none());
    assert!(rec.epoch > e1);
}

/// Spec §7.12: enroll refuses each same-secret case through `check_enrollment`, writes nothing,
/// and prints no secret value.
#[test]
fn enroll_refuses_a_read_secret_that_is_the_write_token_under_another_name() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let sym = format!("{}/sym", f.home.join(SECRETS_DIR).display());
    std::os::unix::fs::symlink(&w, &sym).unwrap();
    let hard = format!("{}/hard", f.home.join(SECRETS_DIR).display());
    std::fs::hard_link(&w, &hard).unwrap();
    let twin = plant(&f, "twin", TOKEN, 0o600);
    for (what, read) in [
        ("same path", w.clone()),
        ("symlink", sym),
        ("hard link", hard),
        ("equal contents", twin),
    ] {
        let (code, out) = go(&f, https_args(&f, split(&w, &read)));
        assert_eq!(code, 1, "{what}: {out}");
        assert!(
            out.contains("the read and write secrets are the same token"),
            "{what}: {out}"
        );
        assert!(
            out.contains("a read credential must be a different token"),
            "{what}: {out}"
        );
        assert!(out.contains("nothing was written"), "{what}: {out}");
        assert!(!out.contains(TOKEN), "{what}: {out}");
        assert_eq!(files(&f), (None, None), "{what}");
    }
}

#[test]
fn two_different_secrets_of_equal_or_different_length_enroll() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    for (i, body) in [
        // One byte off, same length.
        "ghp_write_token_valuf",
        // Different length.
        "ghp_read",
    ]
    .iter()
    .enumerate()
    {
        let r = plant(&f, &format!("read{i}"), body, 0o600);
        let (code, out) = go(&f, https_args(&f, split(&w, &r)));
        assert_eq!(code, 0, "{body}: {out}");
    }
}

/// Spec §6/§7.8: an unusable read secret is `CredentialUnusable`, never `CredentialSameSecret`,
/// even when it is a link to the (usable) write secret's twin with a bad mode.
#[test]
fn an_unusable_read_secret_is_reported_as_unusable_not_as_the_same_token() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let loose = plant(&f, "loose", TOKEN, 0o644);
    let (code, out) = go(&f, https_args(&f, split(&w, &loose)));
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("bad_mode"), "{out}");
    assert!(!out.contains("same token"), "{out}");
    let absent = f
        .home
        .join(SECRETS_DIR)
        .join("absent")
        .display()
        .to_string();
    let (code, out) = go(&f, https_args(&f, split(&w, &absent)));
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("not_found"), "{out}");
    assert!(!out.contains("same token"), "{out}");
    assert_eq!(files(&f), (None, None));
}

/// Spec §7.8: enroll of a single-credential https remote still succeeds; the shared hint is a
/// note, not a refusal, and it names the re-enroll flags.
#[test]
fn a_single_credential_https_enroll_still_succeeds_with_the_shared_hint() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let (code, out) = go(&f, https_args(&f, with_credential(w)));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("note:"), "{out}");
    assert!(out.contains("--read-username"), "{out}");
    assert!(out.contains("--read-secret"), "{out}");
    assert!(!out.contains("refused"), "{out}");
}

#[test]
fn no_shared_hint_for_a_split_or_a_local_enrollment() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let r = plant(&f, "read", READ_TOKEN, 0o600);
    let (_, out) = go(&f, https_args(&f, split(&w, &r)));
    assert!(!out.contains("share one credential"), "{out}");
    let g = fx();
    let (_, out) = go(&g, enroll_args(&g, "alice"));
    assert!(!out.contains("share one credential"), "{out}");
}

/// Spec §7.8: the hint does not change the doctor's exit code; it is reported at `note`.
#[test]
fn doctor_reports_the_shared_hint_without_failing() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    go(&f, https_args(&f, with_credential(w)));
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("note"), "{out}");
    assert!(out.contains("--read-username"), "{out}");
}

/// Spec §7.12: a hand-edited record whose read secret equals the write secret is reported by the
/// doctor with a failing exit code, and no secret value is printed.
#[test]
fn doctor_fails_on_a_hand_edited_record_with_the_same_secret_twice() {
    let f = fx();
    let w = plant(&f, "write", TOKEN, 0o600);
    let r = plant(&f, "read", READ_TOKEN, 0o600);
    go(&f, https_args(&f, split(&w, &r)));
    let (code, _) = doctor_out(&f);
    assert_eq!(code, 0, "a proper split is clean");
    std::fs::write(&r, TOKEN).unwrap();
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("refuse"), "{out}");
    assert!(out.contains("same token"), "{out}");
    assert!(!out.contains(TOKEN) && !out.contains(READ_TOKEN), "{out}");
}
