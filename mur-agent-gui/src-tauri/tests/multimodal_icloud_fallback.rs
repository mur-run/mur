use mur_agent_gui_lib::multimodal::icloud_fallback::{ClipboardSource, icloud_fallback_bytes};

struct StaticClip(Option<Vec<u8>>);

impl ClipboardSource for StaticClip {
    fn read_image(&self) -> Option<Vec<u8>> {
        self.0.clone()
    }
}

#[test]
fn empty_paths_with_clipboard_image_returns_bytes() {
    let clip = StaticClip(Some(b"PNG_BYTES".to_vec()));
    let bytes = icloud_fallback_bytes(&clip).expect("clip image present");
    assert_eq!(bytes, b"PNG_BYTES");
}

#[test]
fn empty_paths_with_no_clipboard_returns_none() {
    let clip = StaticClip(None);
    assert!(icloud_fallback_bytes(&clip).is_none());
}
