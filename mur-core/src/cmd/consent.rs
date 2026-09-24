//! Consent for anything that downloads or installs on the user's behalf.
//!
//! One rule, shared by every such prompt (`mur deep-research setup`,
//! `mur browser setup`): only a literal `yes` counts. `y`, `Y`, `YES` and an
//! empty line or EOF are all "no", so nothing is fetched on a reflexive
//! Enter. This is deliberately stricter than `deps/install.rs::confirm`
//! (`[y/N]`).

use std::io::{BufRead, Write};

use anyhow::Result;

/// Ask, then read one line. `true` only for a trimmed, literal `yes`.
/// Nothing may run on `false`.
pub fn literal_yes(input: &mut dyn BufRead, output: &mut dyn Write) -> Result<bool> {
    write!(output, "Type 'yes' to do this now (anything else = skip): ")?;
    output.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim() == "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(answer: &str) -> (bool, String) {
        let mut out = Vec::new();
        let yes = literal_yes(&mut answer.as_bytes(), &mut out).unwrap();
        (yes, String::from_utf8(out).unwrap())
    }

    #[test]
    fn only_a_literal_yes_is_consent() {
        for a in ["yes", "yes\n", "  yes  \n", "yes\r\n"] {
            assert!(ask(a).0, "{a:?} should be consent");
        }
        for a in ["y\n", "Y\n", "YES\n", "Yes\n", "yess\n", "\n", "", "no\n"] {
            assert!(!ask(a).0, "{a:?} must not be consent");
        }
    }

    #[test]
    fn prompt_says_what_counts() {
        let (_, out) = ask("\n");
        assert_eq!(out, "Type 'yes' to do this now (anything else = skip): ");
    }

    #[test]
    fn reads_only_one_line() {
        let mut input: &[u8] = b"yes\nleftover\n";
        assert!(literal_yes(&mut input, &mut Vec::new()).unwrap());
        assert_eq!(input, b"leftover\n");
    }
}
