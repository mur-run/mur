//! Grounding + coverage checks (spec §5.5).
//! Strips unknown [cit: ...] from streaming tokens (prevents hallucination).

pub struct GroundingFilter {
    valid: std::collections::HashSet<String>,
    buffer: String, // tail buffer to detect brackets across chunk boundaries
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
        FilterOutput {
            forwarded,
            stripped,
        }
    }

    pub fn flush(&mut self) -> FilterOutput {
        let mut out = self.feed("");
        // Drain remaining buffer (no more input coming)
        out.forwarded.push_str(&self.buffer);
        self.buffer.clear();
        out
    }
}

fn find_char_boundary(s: &str, target: usize) -> usize {
    let mut b = target;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
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
}
