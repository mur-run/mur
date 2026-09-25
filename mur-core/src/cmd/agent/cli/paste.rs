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
fn normalize_paste_path(text: &str) -> Option<PathBuf> {
    let mut s = text.trim().trim_matches(['"', '\'']).to_string();
    if let Some(rest) = s.strip_prefix("file://") {
        s = percent_decode(rest)?;
    } else {
        s = s.replace("\\ ", " ");
    }
    let path = PathBuf::from(s);
    path.is_file().then_some(path)
}

/// Full percent-decoding of a `file://` URL path.
///
/// Non-ASCII file names arrive as UTF-8 percent-escapes (a CJK character is
/// three `%XX` bytes), so decoding must happen at the BYTE level and be
/// re-assembled as UTF-8 — decoding escape-by-escape into `char`s mangles
/// every multi-byte name. `None` when the result is not valid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let raw = s.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%'
            && let Some(hex) = raw.get(i + 1..i + 3)
            && let Ok(hex) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
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

/// Rejoin newlines that the terminal *painted* rather than the user typing
/// them (#003).
///
/// Copying a long line out of the transcript copies the pane's grid, so a line
/// the renderer wrapped comes back with a real `\n` in it — pasting a wrapped
/// `mur agent perm allow-read <long path>` split the path in two. The paste
/// itself carries no flag distinguishing the two kinds of newline, so this
/// reconstructs the renderer's decision: ratatui's `Wrap` breaks at
/// whitespace, and it only breaks when the next word does not fit. Therefore
/// a break is a *soft* (painted) one exactly when the line it ends was too
/// full to admit the first word of the next line.
///
/// Conservative by construction — every uncertain case keeps the newline:
///
/// * `width == 0` (nothing rendered yet) → unchanged; an unknown width may
///   never edit a paste.
/// * A line that still had room for the next word was ended by the user.
/// * A blank line, or a next line that is indented or starts a list/quote
///   marker, is deliberate structure: pasted code and Markdown survive intact.
///
/// Rejoining inserts a single space, since the wrap consumed the whitespace it
/// broke at.
pub fn unwrap_soft_breaks(text: &str, width: u16) -> String {
    if width == 0 || !text.contains('\n') {
        return text.to_string();
    }
    let width = width as usize;

    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        let Some(next) = lines.get(i + 1) else {
            continue;
        };

        if soft_break(line, next, width) {
            out.push(' ');
        } else {
            out.push('\n');
        }
    }
    out
}

/// True when the break between `line` and `next` was painted by the wrap.
fn soft_break(line: &str, next: &str, width: usize) -> bool {
    use unicode_width::UnicodeWidthStr;

    // Blank lines are structure, never a wrap artifact.
    if line.trim().is_empty() || next.trim().is_empty() {
        return false;
    }
    // Leading whitespace or a block marker on the next line means the author
    // put it there: indented code, list items, quotes.
    if next.starts_with([' ', '\t'])
        || matches!(
            next.trim_start().as_bytes().first(),
            Some(b'-' | b'*' | b'>' | b'#' | b'|' | b'+')
        )
    {
        return false;
    }
    // The renderer only breaks when the next word will not fit. If it WOULD
    // have fit, the user typed this newline.
    let Some(word) = next.split_whitespace().next() else {
        return false;
    };
    line.width() + 1 + word.width() > width
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
    fn image_from_paste_decodes_non_ascii_file_urls() {
        let dir = std::env::temp_dir();
        let png = dir.join(format!("截圖 測試-{}.png", std::process::id()));
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nfake").unwrap();

        // Finder/terminal hand over a percent-encoded URL: CJK is UTF-8 bytes.
        let encoded: String = png
            .to_str()
            .unwrap()
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        assert!(matches!(
            image_from_paste(&format!("file://{encoded}")),
            Some(("image/png", _))
        ));

        let _ = std::fs::remove_file(&png);
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
