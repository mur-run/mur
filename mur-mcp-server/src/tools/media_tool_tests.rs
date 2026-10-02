use super::*;
#[test]
fn media_tools_registered() {
    let names: Vec<_> = all_tools().into_iter().map(|t| t.name).collect();
    for n in [
        "vlc_open",
        "vlc_playback",
        "vlc_status",
        "scene_explain",
        "video_analyze",
        "watch_start",
        "watch_stop",
        "watch_mute",
        "watch_status",
    ] {
        assert!(names.contains(&n.to_string()), "missing {n}");
    }
}
