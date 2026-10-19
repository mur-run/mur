//! Closest-match hint for a failed `edit_file` lookup.
//!
//! Diagnostic only: the hint is appended to the `not found` error so the
//! model can fix its `old_string` without re-reading the whole file. Nothing
//! here ever feeds back into what gets replaced — matching stays exact.

/// Upper bound on DP cells (`|old| × |text|`) spent hunting for a hint. Keeps
/// the failure path cheap on big files; past it we say why there is no hint.
const MAX_CELLS: usize = 20_000_000;
/// Below this similarity the "closest" span is noise, so we do not show it.
const MIN_SIMILARITY: f64 = 0.5;
/// Snippet limits so a hint never floods the context window.
const MAX_SNIPPET_LINES: usize = 12;
const MAX_SNIPPET_CHARS: usize = 800;

/// Best approximate occurrence of `needle` in `hay`, as char offsets.
struct Hit {
    start: usize,
    end: usize,
    dist: usize,
}

/// Sellers' semi-global edit distance: the needle must be consumed whole,
/// the haystack match may start and end anywhere. Returns (end, distance) of
/// the first best end position.
fn best_end(needle: &[char], hay: &[char]) -> (usize, usize) {
    let m = needle.len();
    let mut col: Vec<usize> = (0..=m).collect();
    let (mut best_end, mut best) = (0, m);
    for (j, &h) in hay.iter().enumerate() {
        let mut diag = col[0]; // row 0 is free: a match can start anywhere
        for i in 1..=m {
            let up = col[i];
            let cost = usize::from(needle[i - 1] != h);
            col[i] = (diag + cost).min(up + 1).min(col[i - 1] + 1);
            diag = up;
        }
        if col[m] < best {
            (best, best_end) = (col[m], j + 1);
        }
    }
    (best_end, best)
}

fn closest(needle: &[char], hay: &[char]) -> Option<Hit> {
    if needle.is_empty() || hay.is_empty() {
        return None;
    }
    let (end, dist) = best_end(needle, hay);
    // A match ending at `end` with distance `dist` starts no earlier than
    // `end - m - dist`; scan that window backwards to find the start.
    let lo = end.saturating_sub(needle.len() + dist);
    let rev_needle: Vec<char> = needle.iter().rev().copied().collect();
    let rev_hay: Vec<char> = hay[lo..end].iter().rev().copied().collect();
    let (back, _) = best_end(&rev_needle, &rev_hay);
    Some(Hit {
        start: end - back,
        end,
        dist,
    })
}

fn squash_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extend a char span outward to whole lines, so the snippet reads naturally.
fn whole_lines(hay: &[char], start: usize, end: usize) -> (usize, usize) {
    let s = hay[..start]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |p| p + 1);
    let e = hay[end..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(hay.len(), |p| end + p);
    (s, e)
}

fn clip(snippet: &str) -> String {
    let mut out: String = snippet
        .lines()
        .take(MAX_SNIPPET_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let mut clipped = snippet.lines().count() > MAX_SNIPPET_LINES;
    if out.chars().count() > MAX_SNIPPET_CHARS {
        out = out.chars().take(MAX_SNIPPET_CHARS).collect();
        clipped = true;
    }
    if clipped {
        out.push_str("\n…");
    }
    out
}

/// First line where `old` and `found` disagree, for a pinpoint message.
fn first_diff(old: &str, found: &str) -> Option<(usize, String, String)> {
    let (a, b): (Vec<_>, Vec<_>) = (old.lines().collect(), found.lines().collect());
    (0..a.len().max(b.len())).find_map(|i| {
        let (x, y) = (a.get(i).copied(), b.get(i).copied());
        (x != y).then(|| {
            (
                i + 1,
                x.unwrap_or("").to_owned(),
                y.unwrap_or("").to_owned(),
            )
        })
    })
}

/// Text to append to a `not found` error. Always returns something, so the
/// model learns why a hint is missing instead of guessing.
pub(super) fn not_found_hint(text: &str, old: &str) -> String {
    let needle: Vec<char> = old.chars().collect();
    let hay: Vec<char> = text.chars().collect();
    if needle.len().saturating_mul(hay.len()) > MAX_CELLS {
        return " (file too large for a closest-match hint — re-read the target range)".into();
    }
    let Some(hit) = closest(&needle, &hay) else {
        return String::new();
    };
    let similarity = 1.0 - hit.dist as f64 / needle.len() as f64;
    if similarity < MIN_SIMILARITY {
        return " (no similar text in the file)".into();
    }
    let (ls, le) = whole_lines(&hay, hit.start, hit.end);
    let span: String = hay[ls..le].iter().collect();
    let first_line = hay[..ls].iter().filter(|&&c| c == '\n').count() + 1;
    let last_line = first_line + span.matches('\n').count();
    let lines = if first_line == last_line {
        format!("line {first_line}")
    } else {
        format!("lines {first_line}-{last_line}")
    };
    let matched: String = hay[hit.start..hit.end].iter().collect();
    let why = if squash_ws(&matched) == squash_ws(old) {
        "differs only in whitespace/indentation".to_owned()
    } else if let Some((n, want, got)) = first_diff(old, &matched) {
        format!("first difference at old_string line {n}: you sent {want:?}, file has {got:?}")
    } else {
        "differs inside a line".to_owned()
    };
    format!(
        "\nclosest match ({:.0}% similar, {lines}; {why}) — matching stays exact, copy the text below verbatim:\n{}",
        similarity * 100.0,
        clip(&span)
    )
}
