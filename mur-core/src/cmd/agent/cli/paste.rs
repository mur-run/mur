//! Bracketed-paste → image detection.
//!
//! Terminals eat Cmd+V and paste the clipboard as bracketed text. For an image
//! that text is the temp-file PATH the terminal wrote (iTerm2 and most others)
//! or a `file://` URL; drag-drop pastes the path too. So a paste that resolves
//! to an existing image file is treated as an inline image, which is what makes
//! Cmd+V (and drag-drop) image paste work without the app ever seeing Cmd+V.

use std::path::{Path, PathBuf};

/// If `text` is the path (or `file://` URL) to an image file, read it and
/// return `(mime, base64)`. `None` for ordinary text, so the caller falls
/// through to inserting it into the input box.
pub fn image_from_paste(text: &str) -> Option<(&'static str, String)> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let path = normalize_paste_path(text)?;
    let mime = image_mime_for(&path)?;
    let bytes = std::fs::read(&path).ok()?;
    (!bytes.is_empty()).then(|| (mime, STANDARD.encode(&bytes)))
}

/// Turn a pasted token into a real file path if it is one: strip a `file://`
/// scheme (with minimal %-decoding), surrounding quotes, and drag-drop's
/// backslash-escaped spaces; accept only an existing regular file.
/// ponytail: decodes just `%20`; widen the %-decode if real paths need it.
fn normalize_paste_path(text: &str) -> Option<PathBuf> {
    let mut s = text.trim().trim_matches(['"', '\'']).to_string();
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.replace("%20", " ");
    } else {
        s = s.replace("\\ ", " ");
    }
    let path = PathBuf::from(s);
    path.is_file().then_some(path)
}

/// Map an image file extension to its MIME type; `None` for non-images.
fn image_mime_for(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_from_paste_accepts_image_files_only() {
        let dir = std::env::temp_dir();
        // Unique per process so parallel test binaries don't collide.
        let png = dir.join(format!("murmur-paste-{}.png", std::process::id()));
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nfake").unwrap();

        // plain path and file:// URL both resolve to the image
        assert!(matches!(
            image_from_paste(png.to_str().unwrap()),
            Some(("image/png", _))
        ));
        let url = format!("file://{}", png.display());
        assert!(matches!(image_from_paste(&url), Some(("image/png", _))));

        // non-image file and ordinary text fall through (None)
        let txt = dir.join(format!("murmur-paste-{}.txt", std::process::id()));
        std::fs::write(&txt, b"hi").unwrap();
        assert_eq!(image_from_paste(txt.to_str().unwrap()), None);
        assert_eq!(image_from_paste("just some pasted words"), None);

        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&txt);
    }

    #[test]
    fn image_mime_covers_common_formats() {
        assert_eq!(image_mime_for(Path::new("a.JPG")), Some("image/jpeg"));
        assert_eq!(image_mime_for(Path::new("shot.jpeg")), Some("image/jpeg"));
        assert_eq!(image_mime_for(Path::new("x.gif")), Some("image/gif"));
        assert_eq!(image_mime_for(Path::new("x.webp")), Some("image/webp"));
        assert_eq!(image_mime_for(Path::new("notes.txt")), None);
        assert_eq!(image_mime_for(Path::new("noext")), None);
    }
}
