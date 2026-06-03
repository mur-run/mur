use crate::types::CompressError;
use tiktoken_rs::CoreBPE;

pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
}

pub struct TiktokenCounter {
    bpe: CoreBPE,
}

impl TiktokenCounter {
    pub fn new() -> Result<Self, CompressError> {
        let bpe =
            tiktoken_rs::cl100k_base().map_err(|e| CompressError::Tokenizer(e.to_string()))?;
        Ok(Self { bpe })
    }
}

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> usize {
        self.bpe.encode_with_special_tokens(text).len()
    }
}

pub struct HeuristicCounter;

impl TokenCounter for HeuristicCounter {
    fn count(&self, text: &str) -> usize {
        text.len().div_ceil(4)
    }
}

/// Returns best counter; never fails.
pub fn default_counter() -> Box<dyn TokenCounter> {
    match TiktokenCounter::new() {
        Ok(c) => Box::new(c),
        Err(_) => Box::new(HeuristicCounter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiktoken_counts_are_positive_and_subword() {
        let c = TiktokenCounter::new().expect("cl100k_base loads");
        // tiktoken may tokenize "hello world" as 2 tokens; allow >=2 for vocab flexibility
        assert!(c.count("hello world") >= 2);
    }

    #[test]
    fn heuristic_is_chars_over_four() {
        let h = HeuristicCounter;
        assert_eq!(h.count("abcd"), 1);
        assert_eq!(h.count("abcde"), 2);
    }
}
