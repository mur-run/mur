//! Grapheme-to-phoneme façade. Kokoro 82M takes phoneme-id sequences
//! as input; we produce them per-language. v1 supports en-* (espeak-ng
//! style mapping) and zh-* (jieba-style word split + per-char map).
//!
//! M1.3.1 ships a deterministic stub mapping sufficient for the
//! inference round-trip and unit tests; the production phoneme tables
//! live in voice-pack metadata (loaded at session-init time) and
//! replace this stub in M1.3.3+ once we wire the Kokoro vocab in.

use anyhow::{Result, bail};

/// Convert text → phoneme-id sequence, dispatching on BCP-47 language.
pub fn text_to_phoneme_ids(text: &str, language: &str) -> Result<Vec<i64>> {
    let lang = language.to_ascii_lowercase();
    match lang.as_str() {
        l if l.starts_with("en") => Ok(english_phonemes(text)),
        l if l.starts_with("zh") => Ok(chinese_phonemes(text)),
        other => bail!("g2p: unsupported language `{other}`"),
    }
}

fn english_phonemes(text: &str) -> Vec<i64> {
    // Stub: ASCII chars become deterministic ids in [1, 127]. Whitespace
    // collapses to a single boundary token (id=1). Real impl loads
    // espeak-ng phoneme table + Kokoro's vocab in M1.3.3.
    let mut out = Vec::with_capacity(text.len());
    let mut last_was_boundary = true;
    for c in text.chars() {
        if c.is_whitespace() {
            if !last_was_boundary {
                out.push(1);
                last_was_boundary = true;
            }
            continue;
        }
        let id = phoneme_stub_id(c);
        if id != 0 {
            out.push(id);
            last_was_boundary = false;
        }
    }
    out
}

fn chinese_phonemes(text: &str) -> Vec<i64> {
    // Stub: per-char ids, no jieba word split yet. Real impl in M1.3.3
    // pairs jieba word boundaries with a Kokoro pinyin→IPA map.
    let mut out = Vec::with_capacity(text.chars().count() * 2);
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        let id = phoneme_stub_id(c);
        if id != 0 {
            out.push(id);
        }
    }
    out
}

/// Stand-in mapping; replaced in M1.3.3 with the real Kokoro vocab.
/// Must be deterministic + non-zero to make round-trip tests stable.
fn phoneme_stub_id(c: char) -> i64 {
    let cp = c as u32 as i64;
    if cp < 128 {
        cp.max(2)
    } else {
        (cp % 256).max(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_yields_no_ids() {
        assert!(text_to_phoneme_ids("", "en-US").unwrap().is_empty());
    }

    #[test]
    fn english_ascii_round_trips() {
        let ids = text_to_phoneme_ids("hi", "en-US").unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|&id| id >= 2));
    }

    #[test]
    fn english_collapses_whitespace_to_single_boundary() {
        let ids = text_to_phoneme_ids("a   b", "en-US").unwrap();
        // a + boundary + b
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn chinese_skips_whitespace() {
        let ids = text_to_phoneme_ids("你 好", "zh-TW").unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn unsupported_language_errors() {
        let r = text_to_phoneme_ids("hola", "es-ES");
        assert!(r.is_err());
    }

    #[test]
    fn dispatch_is_case_insensitive() {
        let lower = text_to_phoneme_ids("hi", "en-us").unwrap();
        let upper = text_to_phoneme_ids("hi", "EN-US").unwrap();
        assert_eq!(lower, upper);
    }
}
