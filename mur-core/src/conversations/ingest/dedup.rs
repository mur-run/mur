//! Stage 2: MinHash 64-bit near-duplicate detection. Threshold 0.85 Jaccard.
//!
//! Uses character 3-grams (trigrams) for shingling. Char-level shingles give
//! high Jaccard for near-identical strings (e.g., differing by one digit) while
//! keeping truly distinct strings well below the threshold.
use std::collections::HashSet;

const NUM_HASHES: usize = 64;
/// Character n-gram size. Char trigrams give Jaccard ≥ 0.90 for strings
/// differing by a single token, vs ~0.01 for semantically distinct strings.
const SHINGLE_SIZE: usize = 3;

pub struct Dedup {
    threshold: f64,
    seen: Vec<[u64; NUM_HASHES]>,
    signature_set: HashSet<[u64; NUM_HASHES]>,
}

impl Dedup {
    pub fn new(threshold: f64) -> Self {
        Self {
            threshold,
            seen: Vec::new(),
            signature_set: HashSet::new(),
        }
    }

    pub fn is_duplicate(&mut self, text: &str) -> bool {
        let sh = shingles(text, SHINGLE_SIZE);
        if sh.len() < 2 {
            return false;
        }
        let sig = minhash_signature(&sh);
        if self.signature_set.contains(&sig) {
            return true;
        }
        for prev in &self.seen {
            if jaccard_estimate(&sig, prev) >= self.threshold {
                return true;
            }
        }
        self.seen.push(sig);
        self.signature_set.insert(sig);
        false
    }
}

fn shingles(text: &str, k: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < k {
        return Vec::new();
    }
    (0..=chars.len() - k)
        .map(|i| chars[i..i + k].iter().collect())
        .collect()
}

fn minhash_signature(shingles: &[String]) -> [u64; NUM_HASHES] {
    let mut sig = [u64::MAX; NUM_HASHES];
    for sh in shingles {
        for (i, slot) in sig.iter_mut().enumerate() {
            let h = hash64(sh, i as u64);
            if h < *slot {
                *slot = h;
            }
        }
    }
    sig
}

fn jaccard_estimate(a: &[u64; NUM_HASHES], b: &[u64; NUM_HASHES]) -> f64 {
    a.iter().zip(b.iter()).filter(|(x, y)| x == y).count() as f64 / NUM_HASHES as f64
}

fn hash64(s: &str, seed: u64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325u64.wrapping_add(seed);
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_dedup() {
        let mut d = Dedup::new(0.85);
        assert!(!d.is_duplicate("cargo build succeeded in 3.2s elapsed"));
        assert!(d.is_duplicate("cargo build succeeded in 3.2s elapsed"));
    }

    #[test]
    fn near_identical_dedup() {
        let mut d = Dedup::new(0.85);
        let a = "warning unused variable x in src/main.rs line 42 column 8 here";
        let b = "warning unused variable x in src/main.rs line 42 column 9 here";
        d.is_duplicate(a);
        assert!(d.is_duplicate(b));
    }

    #[test]
    fn distinct_kept() {
        let mut d = Dedup::new(0.85);
        d.is_duplicate("compile error in src/foo.rs line twenty");
        assert!(!d.is_duplicate("runtime panic at thread main during startup"));
    }

    #[test]
    fn short_text_kept() {
        let mut d = Dedup::new(0.85);
        assert!(!d.is_duplicate("hi"));
        assert!(!d.is_duplicate("hi"));
    }
}
