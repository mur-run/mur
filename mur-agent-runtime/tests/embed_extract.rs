use flate2::Compression;
use flate2::write::GzEncoder;
use mur_agent_runtime::export::extract::extract_embedded_to;
use tempfile::TempDir;

fn build_synthetic_tar() -> Vec<u8> {
    // Minimal in-memory tar.gz of an "agent home" with profile.yaml +
    // sys_prompt.md, just enough to exercise the extractor.
    let mut buf = Vec::new();
    {
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(gz);
        let pf = b"name: synth\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(pf.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, "profile.yaml", &pf[..]).unwrap();

        let sp = b"prompt body";
        let mut h2 = tar::Header::new_gnu();
        h2.set_size(sp.len() as u64);
        h2.set_mode(0o644);
        h2.set_cksum();
        tar.append_data(&mut h2, "sys_prompt.md", &sp[..]).unwrap();

        tar.into_inner().unwrap().finish().unwrap();
    }
    buf
}

#[test]
fn extract_populates_target_dir_with_marker() {
    let base = TempDir::new().unwrap();
    let bytes = build_synthetic_tar();
    let info = extract_embedded_to(&bytes, base.path()).expect("extract");
    assert!(info.fresh, "first extract must be fresh");
    assert!(info.agent_home.join("profile.yaml").exists());
    assert!(info.agent_home.join("sys_prompt.md").exists());
    assert!(
        info.agent_home.join(".extract_digest").exists(),
        "marker must be written"
    );
    let marker = std::fs::read_to_string(info.agent_home.join(".extract_digest")).unwrap();
    assert_eq!(marker.trim(), info.digest);
}

#[test]
fn extract_is_idempotent_when_digest_matches() {
    let base = TempDir::new().unwrap();
    let bytes = build_synthetic_tar();
    let first = extract_embedded_to(&bytes, base.path()).expect("first");
    assert!(first.fresh);
    // Touch a sentinel file inside the extracted dir; if the second call
    // re-extracts, the sentinel will be unaffected (extract reuses dir
    // when digest matches).
    let sentinel = first.agent_home.join("user_added.txt");
    std::fs::write(&sentinel, "keep me").unwrap();

    let second = extract_embedded_to(&bytes, base.path()).expect("second");
    assert!(!second.fresh, "second extract must skip");
    assert_eq!(first.agent_home, second.agent_home);
    assert!(sentinel.exists(), "user_added.txt must be preserved");
}
