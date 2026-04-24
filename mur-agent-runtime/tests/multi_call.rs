use mur_agent_runtime::multi_call::{DispatchError, extract_profile_name};

#[test]
fn extracts_from_symlink_basename() {
    assert_eq!(extract_profile_name("mur_agent_a").unwrap(), "a");
    assert_eq!(
        extract_profile_name("mur_agent_price_hunter").unwrap(),
        "price_hunter"
    );
    assert_eq!(
        extract_profile_name("/opt/homebrew/bin/mur_agent_a").unwrap(),
        "a"
    );
}

#[test]
fn rejects_runtime_basename_without_flag() {
    match extract_profile_name("mur-agent-runtime") {
        Err(DispatchError::BareRuntime) => {}
        other => panic!("expected BareRuntime, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_basename() {
    match extract_profile_name("random-tool") {
        Err(DispatchError::UnknownBasename(_)) => {}
        other => panic!("expected UnknownBasename, got {other:?}"),
    }
}

#[test]
fn strips_windows_exe_suffix() {
    assert_eq!(extract_profile_name("mur_agent_a.exe").unwrap(), "a");
    assert_eq!(
        extract_profile_name(r"C:\bin\mur_agent_a.exe").unwrap(),
        "a"
    );
}
