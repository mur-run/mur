//! Byte-budget helpers for the project instructions block.

/// The longest prefix of `text` within `max` bytes, cut on a char boundary so
/// a CJK or emoji character straddling the limit is dropped, not split.
pub(super) fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Bytes each file may keep out of `cap`, in the same order as `sizes`
/// (spec §5.5). Every file starts with an even share; a file smaller than its
/// share loads whole and hands the rest back to the pool, which is re-split
/// among the files still over. Repeats until nothing more fits whole, then
/// every remaining file gets the final even share.
///
/// A huge nested file therefore cannot crowd out the root's short rules, and
/// a short file is never cut.
pub(super) fn fair_share(sizes: &[usize], cap: usize) -> Vec<usize> {
    let mut out = vec![0; sizes.len()];
    let mut open: Vec<usize> = (0..sizes.len()).collect();
    let mut pool = cap;
    while !open.is_empty() {
        let share = pool / open.len();
        let (fits, over): (Vec<usize>, Vec<usize>) = open.iter().partition(|&&i| sizes[i] <= share);
        if fits.is_empty() {
            for i in over {
                out[i] = share;
            }
            break;
        }
        for i in fits {
            out[i] = sizes[i];
            pool -= sizes[i];
        }
        open = over;
    }
    out
}
