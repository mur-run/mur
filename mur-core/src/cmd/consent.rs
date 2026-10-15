//! Consent for anything that downloads, installs, or widens a permission on
//! the user's behalf.
//!
//! One rule, shared by every such prompt (`mur deep-research setup`,
//! `mur browser setup`): only a literal `yes` counts. `y`, `Y`, `YES` and an
//! empty line or EOF are all "no", so nothing is fetched on a reflexive
//! Enter. This is deliberately stricter than `deps/install.rs::confirm`
//! (`[y/N]`).

use std::io::{BufRead, Write};

use anyhow::{Result, bail};

/// Ask, then read one line. `true` only for a trimmed, literal `yes`.
/// Nothing may run on `false`.
pub fn literal_yes(input: &mut dyn BufRead, output: &mut dyn Write) -> Result<bool> {
    write!(output, "Type 'yes' to do this now (anything else = skip): ")?;
    output.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim() == "yes")
}

/// How a permission grant may be approved, decided ONCE at the entry point.
///
/// Fail-closed: with no terminal to ask (scripts, pipes, and every MUR agent
/// running `mur` through its bash tool), a widening grant is refused unless
/// the caller passed `--yes` explicitly. There is deliberately no env-var
/// override — an agent can set its own environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// `--yes`, or an in-process caller whose own UI was the confirmation.
    Yes,
    /// A person is at the terminal: prompt and read the answer.
    Ask,
    /// Nobody can answer: refuse.
    Impossible,
}

impl Consent {
    /// The prompt goes to stderr and the answer comes from stdin, so those
    /// two — not stdout — decide whether a person can be asked. Redirecting
    /// stdout (`| tee log`) still prompts.
    pub fn detect(yes: bool) -> Self {
        use std::io::IsTerminal;
        if yes {
            Self::Yes
        } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            Self::Ask
        } else {
            Self::Impossible
        }
    }
}

/// Approve a grant whose preview has already been printed. Pure over its
/// streams so all three paths are unit-testable.
pub fn confirm_grant(
    consent: Consent,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    match consent {
        Consent::Yes => Ok(()),
        Consent::Impossible => bail!(
            "this widens network access and needs confirmation, but there is no terminal \
             to ask — review the preview above and re-run with --yes"
        ),
        Consent::Ask if literal_yes(input, output)? => Ok(()),
        Consent::Ask => bail!("cancelled — nothing was changed"),
    }
}

/// [`confirm_grant`] on the real stdin/stderr.
pub fn confirm_grant_on_terminal(yes: bool) -> Result<()> {
    let stdin = std::io::stdin();
    confirm_grant(
        Consent::detect(yes),
        &mut stdin.lock(),
        &mut std::io::stderr(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(c: Consent, answer: &str) -> (Result<()>, String) {
        let mut out = Vec::new();
        let r = confirm_grant(c, &mut answer.as_bytes(), &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn grant_with_yes_flag_never_reads_input() {
        let (r, out) = grant(Consent::Yes, "");
        assert!(r.is_ok());
        assert!(out.is_empty());
    }

    #[test]
    fn grant_without_a_terminal_fails_closed() {
        let (r, out) = grant(Consent::Impossible, "yes\n");
        let err = r.unwrap_err().to_string();
        assert!(err.contains("--yes"), "{err}");
        assert!(err.contains("no terminal"), "{err}");
        assert!(out.is_empty(), "must not prompt when nobody can answer");
    }

    #[test]
    fn grant_on_a_terminal_needs_a_literal_yes() {
        assert!(grant(Consent::Ask, "yes\n").0.is_ok());
        for a in ["y\n", "Y\n", "\n", ""] {
            let err = grant(Consent::Ask, a).0.unwrap_err().to_string();
            assert!(err.contains("nothing was changed"), "{a:?}: {err}");
        }
    }

    #[test]
    fn detect_honours_the_yes_flag_first() {
        assert_eq!(Consent::detect(true), Consent::Yes);
    }

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
