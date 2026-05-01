//! Strip Unicode-tag and bidi-override characters that are commonly
//! abused for prompt-injection smuggling (Riley Goodside et al.).
//!
//! Spec §15 / roadmap §6.1 rule 15. Returns `(scrubbed, dropped_count)`
//! so callers can log "we removed N suspect chars" to the provenance
//! ledger.

/// Scrub Unicode-tag and bidi-override characters from `s`.
/// Returns `(scrubbed_string, dropped_count)`.
pub fn scrub(s: &str) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut dropped = 0usize;
    for c in s.chars() {
        if is_suspect(c) {
            dropped += 1;
        } else {
            out.push(c);
        }
    }
    (out, dropped)
}

fn is_suspect(c: char) -> bool {
    let cp = c as u32;
    // Tag block U+E0000–U+E007F (full block).
    if (0xE0000..=0xE007F).contains(&cp) {
        return true;
    }
    matches!(
        c,
        '\u{200D}'                      // ZWJ
            | '\u{202A}'..='\u{202E}'   // LRE, RLE, PDF, LRO, RLO
            | '\u{2066}'..='\u{2069}'   // LRI, RLI, FSI, PDI
    )
}
