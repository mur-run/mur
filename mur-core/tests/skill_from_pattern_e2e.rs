use mur_common::knowledge::Maturity;
use mur_common::pattern::{Content, Tags};
use mur_common::skill::types::TrustLevel;
use mur_common::skill::{read_from_dir, validate};
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_from_pattern::cmd_from_pattern_with_home;
use mur_core::store::yaml::YamlStore;

fn make_pattern(
    name: &str,
    desc: &str,
    technical: &str,
    principle: &str,
    maturity: Maturity,
) -> mur_common::pattern::Pattern {
    mur_common::pattern::Pattern {
        base: mur_common::knowledge::KnowledgeBase {
            name: name.into(),
            description: desc.into(),
            content: Content::DualLayer {
                technical: technical.into(),
                principle: Some(principle.into()),
            },
            maturity,
            tags: Tags::default(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

#[tokio::test]
async fn promotes_stable_pattern_to_sandboxed_skill() {
    let home = tempfile::tempdir().unwrap();

    // 1. Create a Stable pattern in tempdir.
    let store = YamlStore::new(home.path().join("patterns")).unwrap();
    let p = make_pattern(
        "git-push-flow",
        "Always pull before push",
        "Run git pull --rebase before git push",
        "Prefer rebase over merge to keep history linear",
        Maturity::Stable,
    );
    store.save(&p).unwrap();

    // 2. Run from-pattern (polish=false → no LLM call, no network).
    cmd_from_pattern_with_home(home.path(), "git-push-flow", false)
        .await
        .unwrap();

    // 3. Assert skill.yaml exists and validates.
    let skill_dir = home.path().join("skills/git-push-flow");
    assert!(skill_dir.join("skill.yaml").exists());
    let m = read_from_dir(&skill_dir).unwrap();
    validate(&m).unwrap();
    assert_eq!(m.publisher, "agent:from-pattern");
    assert_eq!(m.version, "0.1.0");

    // 4. Trust entry is Sandboxed and tagged with the right publisher.
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let entry = trust
        .entries
        .values()
        .find(|e| e.name == "git-push-flow")
        .unwrap();
    assert!(matches!(entry.level, TrustLevel::Sandboxed));
    assert_eq!(entry.publisher.as_deref(), Some("agent:from-pattern"));
}

#[tokio::test]
async fn rejects_draft_pattern() {
    let home = tempfile::tempdir().unwrap();
    let store = YamlStore::new(home.path().join("patterns")).unwrap();
    let p = make_pattern("draft-thing", "...", "tech", "princ", Maturity::Draft);
    store.save(&p).unwrap();

    let err = cmd_from_pattern_with_home(home.path(), "draft-thing", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Stable or Canonical"));
}
