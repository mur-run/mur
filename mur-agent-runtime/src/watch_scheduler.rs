//! C-watch — proactive co-watching scene detection (spec §6). Runtime-only.
//!
//! Takes its own VLC snapshot over HTTP, computes a difference-hash (dHash), and on a
//! large change injects a short text turn into the `TaskRunner`. Cannot call `mur-core`
//! (would cycle), so it relies on `mur_common::media` shared types + the agent's own
//! `scene_explain` tool to actually narrate.

/// Hamming distance between two 64-bit perceptual hashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Compute a dHash from a row-major grayscale buffer of size `w`×`h`.
/// Sets one bit per horizontally-adjacent pair (`right > left`). For 9×8 ⇒ 64 bits.
pub fn dhash_from_luma(luma: &[u8], w: usize, h: usize) -> u64 {
    let mut hash = 0u64;
    let mut bit = 0u32;
    for y in 0..h {
        for x in 0..w.saturating_sub(1) {
            let left = luma[y * w + x];
            let right = luma[y * w + x + 1];
            if right > left {
                hash |= 1u64 << bit;
            }
            bit += 1;
        }
    }
    hash
}

use mur_common::media::Consent;

/// What the scheduler should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Narrate,
    AskConsent,
    Skip,
}

/// Pure gate — no I/O. Order of checks: mute → quiet → magnitude → cooldown → consent.
#[allow(clippy::too_many_arguments)]
pub fn should_interject(
    now_ms: i64,
    last_interjection_ms: i64,
    cooldown_ms: i64,
    distance: u32,
    threshold: u32,
    muted: bool,
    quiet_now: bool,
    consent: Consent,
) -> Decision {
    if muted || quiet_now {
        return Decision::Skip;
    }
    if distance < threshold {
        return Decision::Skip;
    }
    if last_interjection_ms != 0 && now_ms.saturating_sub(last_interjection_ms) < cooldown_ms {
        return Decision::Skip;
    }
    match consent {
        Consent::Unasked => Decision::AskConsent,
        Consent::Granted => Decision::Narrate,
        Consent::Declined => Decision::Skip,
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    const CD: i64 = 45_000;
    const TH: u32 = 18;

    #[test]
    fn small_change_skips() {
        assert_eq!(
            should_interject(100_000, 0, CD, 5, TH, false, false, Consent::Granted),
            Decision::Skip
        );
    }

    #[test]
    fn first_big_change_asks_consent() {
        assert_eq!(
            should_interject(100_000, 0, CD, 30, TH, false, false, Consent::Unasked),
            Decision::AskConsent
        );
    }

    #[test]
    fn granted_big_change_narrates() {
        assert_eq!(
            should_interject(100_000, 0, CD, 30, TH, false, false, Consent::Granted),
            Decision::Narrate
        );
    }

    #[test]
    fn cooldown_blocks() {
        // fired 10s ago, cooldown 45s ⇒ skip
        assert_eq!(
            should_interject(110_000, 100_000, CD, 30, TH, false, false, Consent::Granted),
            Decision::Skip
        );
    }

    #[test]
    fn muted_quiet_declined_all_skip() {
        assert_eq!(should_interject(200_000, 0, CD, 99, TH, true, false, Consent::Granted), Decision::Skip);
        assert_eq!(should_interject(200_000, 0, CD, 99, TH, false, true, Consent::Granted), Decision::Skip);
        assert_eq!(should_interject(200_000, 0, CD, 99, TH, false, false, Consent::Declined), Decision::Skip);
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming(0b0000, 0b0000), 0);
        assert_eq!(hamming(0b1010, 0b0001), 3);
    }

    #[test]
    fn dhash_detects_horizontal_gradient() {
        // 9x8 ascending rows ⇒ every pair has right > left ⇒ all 64 bits set.
        let mut luma = vec![0u8; 9 * 8];
        for y in 0..8 {
            for x in 0..9 {
                luma[y * 9 + x] = (x * 20) as u8;
            }
        }
        assert_eq!(dhash_from_luma(&luma, 9, 8), u64::MAX);

        // A flat image ⇒ no bit set ⇒ maximally different from the gradient.
        let flat = vec![100u8; 9 * 8];
        assert_eq!(dhash_from_luma(&flat, 9, 8), 0);
        assert_eq!(hamming(u64::MAX, 0), 64);
    }
}
