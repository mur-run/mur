//! Sentence splitter — streams the LLM's output into sentence-sized
//! chunks for incremental synthesis. The first sentence reaches TTS
//! before the LLM finishes generating, which is the primary lever for
//! the "first audio chunk ≤ 250 ms" target (roadmap §4.1).
//!
//! Splits on `[.!?。！？]`. Latin-script punctuation requires the next
//! char to be whitespace or EOF (handles "U.S." not splitting). CJK
//! punctuation splits immediately on next char (no whitespace
//! convention in CJK script).

pub struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self { buf: String::new() }
    }

    /// Append raw streaming text; returns 0+ complete sentences.
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buf.push_str(chunk);
        let mut out = vec![];
        while let Some((sent, rest)) = split_first(&self.buf) {
            out.push(sent);
            self.buf = rest;
        }
        out
    }

    /// At end of stream: emit anything still buffered.
    pub fn flush(&mut self) -> Option<String> {
        if self.buf.trim().is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf).trim().to_string())
        }
    }
}

impl Default for SentenceSplitter {
    fn default() -> Self {
        Self::new()
    }
}

fn split_first(s: &str) -> Option<(String, String)> {
    let mut latin_punct_end: Option<usize> = None;
    for (idx, ch) in s.char_indices() {
        let cjk_punct = matches!(ch, '。' | '！' | '？');
        let latin_punct = matches!(ch, '.' | '!' | '?');

        if cjk_punct {
            // CJK punctuation is a boundary on its own — split as soon
            // as we consume it, no whitespace requirement (CJK script
            // doesn't use whitespace between sentences).
            let end = idx + ch.len_utf8();
            let (head, tail) = s.split_at(end);
            return Some((head.trim().to_string(), tail.trim_start().to_string()));
        }

        if latin_punct {
            latin_punct_end = Some(idx + ch.len_utf8());
            continue;
        }

        if let Some(p) = latin_punct_end {
            if ch.is_whitespace() {
                let (head, tail) = s.split_at(p);
                return Some((head.trim().to_string(), tail.trim_start().to_string()));
            }
            latin_punct_end = None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_period_splits_on_whitespace() {
        let mut s = SentenceSplitter::new();
        let out = s.push("Hello. World!");
        // First sentence emits; "World!" stays buffered (no trailing
        // whitespace yet to confirm sentence end vs abbreviation).
        assert_eq!(out, vec!["Hello.".to_string()]);
        let out2 = s.push(" again");
        // Now "World!" gets a trailing whitespace → emits.
        assert_eq!(out2, vec!["World!".to_string()]);
        assert_eq!(s.flush().as_deref(), Some("again"));
    }

    #[test]
    fn chinese_full_stop_splits_without_whitespace() {
        let mut s = SentenceSplitter::new();
        let out = s.push("你好。世界！");
        assert_eq!(out, vec!["你好。".to_string(), "世界！".to_string()]);
    }

    #[test]
    fn streaming_partial_input_buffers_until_terminator() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("This is").is_empty());
        assert!(s.push(" not done").is_empty());
        let out = s.push(". Now done!");
        assert_eq!(out.len(), 1);
        assert!(out[0].ends_with("done."));
    }

    #[test]
    fn flush_emits_trailing_partial_sentence() {
        let mut s = SentenceSplitter::new();
        s.push("Hello world");
        assert_eq!(s.flush().as_deref(), Some("Hello world"));
    }

    #[test]
    fn flush_returns_none_for_empty_buffer() {
        let mut s = SentenceSplitter::new();
        assert!(s.flush().is_none());
    }

    #[test]
    fn no_split_on_abbreviation_dot() {
        // "U.S." is buffered until a whitespace boundary appears.
        // Limitation accepted: this stub treats every '.' followed
        // by whitespace as a sentence boundary; real impl with
        // abbreviation list lands in M1.3.3+.
        let mut s = SentenceSplitter::new();
        let out = s.push("Hello.");
        // No trailing whitespace yet → still buffered.
        assert!(out.is_empty());
    }
}
