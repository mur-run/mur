use std::collections::BTreeSet;
use std::path::Path;

use super::*;

fn flags(with_serena: bool, lsp: &[&str]) -> Flags {
    Flags {
        with_serena,
        no_ast_grep: false,
        lsp: lsp.iter().map(|s| s.to_string()).collect(),
    }
}

/// Every runtime prerequisite on PATH.
fn all_tools() -> Detected {
    let mut on_path = BTreeSet::new();
    for l in Lang::ALL {
        for t in l.prerequisites() {
            on_path.insert(t.to_string());
        }
    }
    on_path.insert(UV.to_string());
    Detected { on_path }
}

fn row(p: &Plan, lang: Lang) -> &LspRow {
    p.lsp.iter().find(|r| r.lang == lang).expect("row listed")
}

fn home() -> &'static Path {
    Path::new("/mh")
}

#[test]
fn default_installs_ast_grep_only() {
    let p = plan(home(), &flags(false, &[]), &all_tools()).unwrap();
    let names: Vec<_> = p.install.iter().map(|r| r.name).collect();
    assert_eq!(names, vec![AST_GREP]);
    assert_eq!(
        p.install[0].version,
        mur_common::config::AST_GREP_PINNED_VERSION
    );
    assert!(p.lsp.is_empty());
    assert!(p.permissions.is_empty());
}

#[test]
fn no_ast_grep_and_no_serena_is_an_empty_plan() {
    let mut f = flags(false, &[]);
    f.no_ast_grep = true;
    let p = plan(home(), &f, &all_tools()).unwrap();
    assert!(p.install.is_empty() && p.lsp.is_empty() && p.permissions.is_empty());
}

#[test]
fn lsp_without_serena_is_refused() {
    let e = plan(home(), &flags(false, &["python"]), &all_tools()).unwrap_err();
    assert!(e.to_string().contains("--with-serena"), "{e}");
}

#[test]
fn serena_defaults_enable_low_and_medium_only() {
    let p = plan(home(), &flags(true, &[]), &all_tools()).unwrap();
    for l in [Lang::Python, Lang::Php, Lang::Lua, Lang::Cpp] {
        assert_eq!(row(&p, l).status, LspStatus::Enabled, "{l:?}");
    }
    for l in [
        Lang::Rust,
        Lang::Kotlin,
        Lang::TypeScript,
        Lang::Go,
        Lang::Java,
        Lang::Swift,
        Lang::Ruby,
    ] {
        assert_eq!(
            row(&p, l).status,
            LspStatus::Skipped {
                enable_flag: format!("--lsp {}", l.flag())
            },
            "{l:?}"
        );
    }
}

#[test]
fn tiers_match_the_merged_table() {
    use Tier::*;
    let want = [
        (Lang::Python, Low),
        (Lang::Php, Low),
        (Lang::Lua, Low),
        (Lang::Cpp, MediumContained),
        (Lang::Rust, High),
        (Lang::Kotlin, High),
        (Lang::TypeScript, High),
        (Lang::Go, High),
        (Lang::Java, High),
        (Lang::Swift, High),
        (Lang::Ruby, High),
    ];
    assert_eq!(Lang::ALL.len(), want.len());
    for (l, t) in want {
        assert_eq!(l.tier(), t, "{l:?}");
    }
}

#[test]
fn explicit_lsp_is_an_allow_list() {
    let p = plan(home(), &flags(true, &["rust"]), &all_tools()).unwrap();
    assert_eq!(row(&p, Lang::Rust).status, LspStatus::Enabled);
    // A listed Low language is no longer on once --lsp names others.
    assert!(matches!(
        row(&p, Lang::Python).status,
        LspStatus::Skipped { .. }
    ));
}

#[test]
fn rust_full_is_the_same_as_rust_and_says_so() {
    let a = plan(home(), &flags(true, &["rust"]), &all_tools()).unwrap();
    let b = plan(home(), &flags(true, &["rust-full"]), &all_tools()).unwrap();
    assert_eq!(a, b);
    let note = row(&a, Lang::Rust).note.unwrap();
    assert!(
        note.contains("rust-full") && note.contains("build scripts"),
        "{note}"
    );
}

#[test]
fn evidence_rows_carry_the_mandated_warning() {
    let p = plan(
        home(),
        &flags(true, &["kotlin", "typescript"]),
        &all_tools(),
    )
    .unwrap();
    assert!(
        row(&p, Lang::Kotlin)
            .note
            .unwrap()
            .contains("runs its build scripts")
    );
    assert!(
        row(&p, Lang::TypeScript)
            .note
            .unwrap()
            .contains("runs its own TypeScript")
    );
}

#[test]
fn untiered_language_is_refused_by_name() {
    for l in ["dart", "bash", "csharp", "fsharp", "scala", "elixir"] {
        let e = plan(home(), &flags(true, &[l]), &all_tools()).unwrap_err();
        assert!(e.to_string().contains("not offered"), "{l}: {e}");
    }
}

#[test]
fn unknown_language_is_refused() {
    let e = plan(home(), &flags(true, &["cobol"]), &all_tools()).unwrap_err();
    assert!(e.to_string().contains("cobol"), "{e}");
}

#[test]
fn lsp_flag_is_case_insensitive() {
    let p = plan(home(), &flags(true, &["Python"]), &all_tools()).unwrap();
    assert_eq!(row(&p, Lang::Python).status, LspStatus::Enabled);
}

#[test]
fn missing_prerequisite_disables_with_a_visible_line() {
    let mut d = all_tools();
    d.on_path.remove("node");
    let p = plan(home(), &flags(true, &["php", "python"]), &d).unwrap();
    assert_eq!(
        row(&p, Lang::Php).status,
        LspStatus::Disabled {
            missing: "node".into()
        }
    );
    assert_eq!(row(&p, Lang::Python).status, LspStatus::Enabled);
    assert_eq!(
        p.disabled_lines(),
        vec!["php disabled: node not found on PATH"]
    );
}

#[test]
fn missing_uv_blocks_serena_and_every_language() {
    let mut d = all_tools();
    d.on_path.remove(UV);
    let p = plan(home(), &flags(true, &[]), &d).unwrap();
    let serena = p.install.iter().find(|r| r.name == SERENA).unwrap();
    assert_eq!(serena.missing, Some(UV));
    assert!(
        p.lsp
            .iter()
            .all(|r| !matches!(r.status, LspStatus::Enabled)),
        "no language may be on without serena"
    );
    assert!(
        p.disabled_lines()
            .contains(&"serena disabled: uv not found on PATH".to_string())
    );
    assert!(
        p.permissions.is_empty(),
        "nothing to grant when serena is off"
    );
}

#[test]
fn permissions_cover_serena_and_enabled_languages_only() {
    let p = plan(home(), &flags(true, &["go", "python"]), &all_tools()).unwrap();
    let spawn: BTreeSet<_> = p
        .permissions
        .iter()
        .filter_map(|g| match g {
            Permission::Spawn(b) => Some(b.as_str()),
            _ => None,
        })
        .collect();
    // uv is install-time only (P3-D2); uvx is what serena spawns for pyright.
    assert_eq!(spawn, BTreeSet::from(["go", "gopls", "uvx"]));
    assert!(p.permissions.contains(&Permission::Read(
        home().join("tools").join(SERENA).join(SERENA_PIN)
    )));
}

#[test]
fn serena_install_row_is_under_mur_home() {
    let p = plan(home(), &flags(true, &[]), &all_tools()).unwrap();
    let serena = p.install.iter().find(|r| r.name == SERENA).unwrap();
    assert_eq!(serena.version, SERENA_PIN);
    assert_eq!(
        serena.dir,
        home().join("tools").join(SERENA).join(SERENA_PIN)
    );
    assert!(serena.missing.is_none());
}

#[test]
fn duplicate_lsp_flags_collapse() {
    let a = plan(home(), &flags(true, &["go", "go"]), &all_tools()).unwrap();
    let b = plan(home(), &flags(true, &["go"]), &all_tools()).unwrap();
    assert_eq!(a, b);
}
