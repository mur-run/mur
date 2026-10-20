//! Shell control flow and visible variables, for [`super::classify_inner`].
//!
//! `for n in a b; do grep -c x $n; done` is a read: the loop header binds a
//! name to literal words, `do`/`done` are keywords, and the only command is a
//! `grep`. Before this pass every segment went to the head check as written,
//! so `for`, `do grep …` and `done` were three unknown heads and the whole
//! loop fell out of the read lane.
//!
//! This pass rewrites the segment list into plain simple commands:
//!
//! - Leading keywords (`do`, `then`, `else`, `elif`, `if`, `while`, `until`,
//!   `!`) are stripped; the condition of an `if`/`while` IS a command and is
//!   checked like any other. A bare `done` / `fi` is dropped.
//! - A `for NAME in WORDS` header and a `name=value` assignment bind a name to
//!   literal values and run nothing, so they are dropped — but only when the
//!   values are visible: no `$`, and no value that could start with `-` once
//!   expanded (a leading glob or dash), because a value that becomes a flag
//!   (`rg --pre=CMD`) is exactly how a read is turned into an exec.
//! - A word that is exactly `$NAME` / `${NAME}` for a bound name is replaced
//!   by each of its values, and the segment is checked once per value.
//!
//! Assignments are accepted only for lowercase names. Tool-steering
//! environment variables (`PATH`, `GIT_EXTERNAL_DIFF`, `LESSOPEN`, …) are
//! uppercase, and a plain assignment to an already-exported name changes what
//! every later child sees.
//!
//! Anything else — an unbound `$X`, an unknown keyword form — is left in
//! place, where the head check refuses it. This pass can only remove words it
//! has proved run nothing; it never marks a command read on its own.

use std::collections::HashMap;

use super::Word;

/// Keywords that may open a simple command and are not themselves a command.
const LEADING_KEYWORDS: &[&str] = &["do", "then", "else", "elif", "if", "while", "until", "!"];

/// Keywords that close a block and must stand alone in their segment.
const CLOSING_KEYWORDS: &[&str] = &["done", "fi"];

/// More combinations than this is not a command a reviewer reads; refuse.
const MAX_EXPANSIONS: usize = 64;

fn is_name(s: &str, lowercase_only: bool) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| {
        c == '_' || c.is_ascii_alphabetic() && (!lowercase_only || c.is_ascii_lowercase())
    }) && cs.all(|c| {
        c == '_'
            || c.is_ascii_digit()
            || c.is_ascii_alphabetic() && (!lowercase_only || c.is_ascii_lowercase())
    })
}

/// A value whose expansion is the literal text itself, and cannot become a flag.
fn visible_value(v: &str) -> bool {
    !v.contains('$') && !v.starts_with(['-', '*', '?', '['])
}

/// `$NAME` or `${NAME}` → `NAME`.
fn var_ref(w: &str) -> Option<&str> {
    let inner = w.strip_prefix('$')?;
    let name = inner
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(inner);
    is_name(name, false).then_some(name)
}

fn strip_keywords(seg: &[Word]) -> &[Word] {
    let mut s = seg;
    while let Some(first) = s.first()
        && !first.quoted
        && LEADING_KEYWORDS.contains(&first.text.as_str())
    {
        s = &s[1..];
    }
    s
}

/// What a segment binds, if it is a binding and nothing else.
fn binding(seg: &[Word]) -> Option<(String, Vec<String>)> {
    let head = seg.first()?;
    if head.quoted {
        return None;
    }
    if head.text == "for" {
        let name = seg.get(1)?;
        if name.quoted || !is_name(&name.text, false) || seg.get(2)?.text != "in" {
            return None;
        }
        let vals: Vec<String> = seg[3..].iter().map(|w| w.text.clone()).collect();
        return vals
            .iter()
            .all(|v| visible_value(v))
            .then(|| (name.text.clone(), vals));
    }
    let assign = match (head.text.as_str(), seg.len()) {
        ("export", 2) => &seg[1],
        (_, 1) => head,
        _ => return None,
    };
    let (k, v) = assign.text.split_once('=')?;
    (is_name(k, true) && visible_value(v)).then(|| (k.to_string(), vec![v.to_string()]))
}

/// Rewrite `segs` into simple commands, or `None` when the shape is refused.
pub(super) fn expand(segs: Vec<Vec<Word>>) -> Option<Vec<Vec<Word>>> {
    let mut vars: HashMap<String, Vec<String>> = HashMap::new();
    let mut body: Vec<Vec<Word>> = Vec::new();
    for seg in &segs {
        let s = strip_keywords(seg);
        if s.is_empty() {
            continue;
        }
        if CLOSING_KEYWORDS.contains(&s[0].text.as_str()) && !s[0].quoted {
            if s.len() > 1 {
                return None; // `done <file`, `fi x` — not a shape this pass models
            }
            continue;
        }
        if let Some((name, vals)) = binding(s) {
            vars.entry(name).or_default().extend(vals);
            continue;
        }
        body.push(s.to_vec());
    }

    let mut out = Vec::new();
    for seg in body {
        let mut combos: Vec<Vec<Word>> = vec![Vec::new()];
        for w in seg {
            let vals = var_ref(&w.text).and_then(|n| vars.get(n));
            match vals {
                Some(vals) if !vals.is_empty() => {
                    if combos.len() * vals.len() > MAX_EXPANSIONS {
                        return None;
                    }
                    combos = combos
                        .into_iter()
                        .flat_map(|c| {
                            vals.iter().map(move |v| {
                                let mut c = c.clone();
                                c.push(Word {
                                    text: v.clone(),
                                    quoted: w.quoted,
                                });
                                c
                            })
                        })
                        .collect();
                }
                _ => combos.iter_mut().for_each(|c| c.push(w.clone())),
            }
        }
        out.extend(combos);
    }
    Some(out)
}
