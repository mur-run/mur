//! `prepare` is pure, so each branch is pinned here without a home dir.

use std::path::Path;

use super::*;
use crate::cmd::code_nav::plan::{Detected, Flags, UV};

fn plan_with(flags: Flags, on_path: &[&str]) -> plan::Plan {
    let detected = Detected {
        on_path: on_path.iter().map(|s| s.to_string()).collect(),
    };
    plan::plan(Path::new("/mh"), &flags, &detected).unwrap()
}

fn serena_flags(lsp: &[&str]) -> Flags {
    Flags {
        with_serena: true,
        lsp: lsp.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn ast_grep_only_installs_ast_grep_and_no_serena() {
    let p = plan_with(Flags::default(), &[]);
    let got = prepare("serena", &p, None, Manifest::default());
    assert_eq!(
        got.ast_grep,
        Some(mur_common::config::AST_GREP_PINNED_VERSION)
    );
    assert_eq!(got.serena, SerenaStep::Off);
}

#[test]
fn no_ast_grep_flag_skips_it() {
    let flags = Flags {
        no_ast_grep: true,
        ..serena_flags(&[])
    };
    let p = plan_with(flags, &[UV]);
    let got = prepare("serena", &p, Some(Path::new("/repo")), Manifest::default());
    assert_eq!(got.ast_grep, None);
    assert!(matches!(got.serena, SerenaStep::Install(_)));
}

#[test]
fn serena_without_uv_is_no_uv_and_records_why() {
    let p = plan_with(serena_flags(&[]), &[]);
    let got = prepare("serena", &p, Some(Path::new("/repo")), Manifest::default());
    assert_eq!(got.serena, SerenaStep::NoUv);
    assert_eq!(got.manifest.disabled, p.disabled_lines());
    assert!(
        got.manifest.disabled.iter().any(|l| l.contains(UV)),
        "{:?}",
        got.manifest.disabled
    );
}

#[test]
fn serena_steps_follow_the_plan_rows() {
    let p = plan_with(
        serena_flags(&["python", "php", "vue"]),
        &[UV, "node", "npm"],
    );
    let SerenaStep::Install(s) =
        prepare("site", &p, Some(Path::new("/repo")), Manifest::default()).serena
    else {
        panic!("serena should install");
    };
    assert_eq!(s.project, Path::new("/repo"));
    assert!(s.pyright && s.intelephense && s.vue);
    // Vue implies TypeScript, so its server is installed too.
    assert!(s.typescript);
    assert_eq!(s.langs, entry_langs(&Manifest::default(), "site", &p));
}

#[test]
fn langs_union_with_the_entry_s_prior_languages() {
    let mut prior = Manifest::default();
    prior.set_languages("site", ["lua".to_owned()]);
    let p = plan_with(serena_flags(&["rust"]), &[UV, "rust-analyzer"]);
    let SerenaStep::Install(s) = prepare("site", &p, Some(Path::new("/r")), prior.clone()).serena
    else {
        panic!("serena should install");
    };
    assert_eq!(s.langs, entry_langs(&prior, "site", &p));
    assert!(s.langs.contains(&Lang::Lua), "never revokes: {:?}", s.langs);
}

#[test]
fn prepare_keeps_the_prior_manifest_but_this_plan_s_disabled_lines() {
    let mut prior = Manifest::default();
    prior.installed.insert("x@1".into());
    prior.disabled = vec!["stale".into()];
    let p = plan_with(serena_flags(&[]), &[UV]);
    let got = prepare("serena", &p, Some(Path::new("/r")), prior.clone());
    assert_eq!(got.manifest.installed, prior.installed);
    assert_eq!(got.manifest.disabled, p.disabled_lines());
}
