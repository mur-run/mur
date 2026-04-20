//! Grounding + coverage checks (spec §5.5).
//! Strips unknown [cit: ...] from streaming tokens (prevents hallucination).

pub struct GroundingFilter {
    valid: std::collections::HashSet<String>,
    buffer: String,        // tail buffer to detect brackets across chunk boundaries
    strict: bool,          // enforce citation coverage when true
    forwarded_all: String, // accumulates all forwarded text for coverage_check
}

pub struct FilterOutput {
    pub forwarded: String,
    pub stripped: Vec<String>,
}

impl GroundingFilter {
    pub fn new(valid: Vec<String>) -> Self {
        Self {
            valid: valid.into_iter().collect(),
            buffer: String::new(),
            strict: false,
            forwarded_all: String::new(),
        }
    }

    pub fn new_strict(valid: Vec<String>) -> Self {
        Self {
            valid: valid.into_iter().collect(),
            buffer: String::new(),
            strict: true,
            forwarded_all: String::new(),
        }
    }

    pub fn feed(&mut self, token: &str) -> FilterOutput {
        self.buffer.push_str(token);
        let mut forwarded = String::new();
        let mut stripped = Vec::new();
        loop {
            // If no open bracket, we can flush everything up to the last 64 chars
            // (hold those back in case an opening bracket arrives split across tokens).
            match self.buffer.find("[cit:") {
                None => {
                    if self.buffer.len() > 64 {
                        let flush_upto = self.buffer.len() - 64;
                        let boundary = find_char_boundary(&self.buffer, flush_upto);
                        forwarded.push_str(&self.buffer[..boundary]);
                        self.buffer.drain(..boundary);
                    }
                    break;
                }
                Some(start) => {
                    // Flush prefix before the bracket.
                    if start > 0 {
                        forwarded.push_str(&self.buffer[..start]);
                        self.buffer.drain(..start);
                    }
                    // Look for the closing ']'.
                    match self.buffer.find(']') {
                        None => {
                            // Bracket incomplete — wait for more input.
                            break;
                        }
                        Some(end) => {
                            let candidate = &self.buffer[..=end];
                            if self.valid.contains(candidate) {
                                forwarded.push_str(candidate);
                            } else {
                                stripped.push(candidate.to_string());
                            }
                            self.buffer.drain(..=end);
                        }
                    }
                }
            }
        }
        self.forwarded_all.push_str(&forwarded);
        FilterOutput {
            forwarded,
            stripped,
        }
    }

    pub fn flush(&mut self) -> FilterOutput {
        let mut out = self.feed("");
        // Drain remaining buffer (no more input coming)
        out.forwarded.push_str(&self.buffer);
        self.forwarded_all.push_str(&self.buffer);
        self.buffer.clear();
        out
    }

    /// Under strict mode, return Err if any claim sentence in the forwarded
    /// text lacks an adjacent [cit: ...] anchor. Returns Ok(()) under
    /// non-strict mode or when coverage is satisfied.
    pub fn coverage_check(&self) -> Result<(), String> {
        if !self.strict {
            return Ok(());
        }
        for sentence in split_sentences(&self.forwarded_all) {
            let words = sentence.split_whitespace().count();
            if words < 8 {
                continue;
            }
            if !sentence.contains("[cit:") {
                return Err(format!(
                    "strict citations: un-cited claim: \"{}\"",
                    sentence.chars().take(120).collect::<String>()
                ));
            }
        }
        Ok(())
    }
}

fn find_char_boundary(s: &str, target: usize) -> usize {
    let mut b = target;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

fn split_sentences(text: &str) -> Vec<&str> {
    // Keep it simple: split on '.', '!', '?' followed by whitespace/EOF.
    // Phase 2C heuristic — not a full NLP sentence splitter.
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' || c == b'!' || c == b'?' {
            let end = i + 1;
            let after = bytes.get(end).copied().unwrap_or(b' ');
            if after.is_ascii_whitespace() || end == bytes.len() {
                let seg = text[start..end].trim();
                if !seg.is_empty() {
                    out.push(seg);
                }
                start = end;
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        let tail = text[start..].trim();
        if !tail.is_empty() {
            out.push(tail);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_known_citation_verbatim() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        let out = f.feed("Answer [cit: 2026-04-19 cc/a:L1] done.");
        let drain = f.flush();
        assert!((out.forwarded + &drain.forwarded).contains("[cit: 2026-04-19 cc/a:L1]"));
        assert!(out.stripped.is_empty() && drain.stripped.is_empty());
    }

    #[test]
    fn strips_unknown_citation() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        let out = f.feed("Claim [cit: 2099-01-01 fake:L0] done.");
        let drain = f.flush();
        let combined = out.forwarded + &drain.forwarded;
        assert!(!combined.contains("[cit: 2099-01-01 fake:L0]"));
        assert_eq!(out.stripped.len() + drain.stripped.len(), 1);
    }

    #[test]
    fn handles_bracket_split_across_tokens() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        let mut combined = String::new();
        combined.push_str(&f.feed("start [c").forwarded);
        combined.push_str(&f.feed("it: 2026-04-19 cc/a").forwarded);
        combined.push_str(&f.feed(":L1] end").forwarded);
        combined.push_str(&f.flush().forwarded);
        assert!(combined.contains("[cit: 2026-04-19 cc/a:L1]"));
    }

    #[test]
    fn strict_accepts_fully_cited_text() {
        let mut f = GroundingFilter::new_strict(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        f.feed("The user decided to refactor the pipeline [cit: 2026-04-19 cc/a:L1]. Done.");
        f.flush();
        assert!(f.coverage_check().is_ok());
    }

    #[test]
    fn strict_rejects_uncited_claim() {
        let mut f = GroundingFilter::new_strict(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        f.feed("The user decided to refactor everything at once without testing. End.");
        f.flush();
        let r = f.coverage_check();
        assert!(r.is_err(), "expected strict rejection, got {:?}", r);
    }

    #[test]
    fn strict_ignores_short_sentences() {
        let mut f = GroundingFilter::new_strict(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        f.feed("Yes. Done. Works [cit: 2026-04-19 cc/a:L1].");
        f.flush();
        // Short sentences "Yes." and "Done." are skipped; the longer one is cited.
        assert!(f.coverage_check().is_ok());
    }

    #[test]
    fn non_strict_mode_never_fails() {
        let mut f = GroundingFilter::new(vec!["[cit: 2026-04-19 cc/a:L1]".into()]);
        f.feed("Long uncited claim about many things we built yesterday morning.");
        f.flush();
        assert!(f.coverage_check().is_ok()); // non-strict — no coverage enforcement
    }
}
