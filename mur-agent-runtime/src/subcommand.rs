//! CLI subcommand / flag parsing for the agent runtime binary.

/// Return the value following `flag` in `args`, supporting both
/// `--flag value` and `--flag=value` forms. Returns the first match.
pub fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let eq_prefix = format!("{flag}=");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&eq_prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Return true if any of `flags` appears as a standalone argument in `args`.
/// Used to detect `--help`/`-h`/`--version`/`-V` before entering the serve loop.
pub fn has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|a| flags.iter().any(|f| a == f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn space_separated_form() {
        let a = args(&["mur-agent-runtime", "--load", "coach.muragent"]);
        assert_eq!(flag_value(&a, "--load"), Some("coach.muragent".to_string()));
    }

    #[test]
    fn equals_form() {
        let a = args(&["mur-agent-runtime", "--model=anthropic_opus_4_7"]);
        assert_eq!(
            flag_value(&a, "--model"),
            Some("anthropic_opus_4_7".to_string())
        );
    }

    #[test]
    fn absent_flag_is_none() {
        let a = args(&["mur-agent-runtime"]);
        assert_eq!(flag_value(&a, "--load"), None);
    }

    #[test]
    fn has_flag_detects_help_and_version() {
        assert!(has_flag(&args(&["x", "--help"]), &["--help", "-h"]));
        assert!(has_flag(&args(&["x", "-h"]), &["--help", "-h"]));
        assert!(has_flag(&args(&["x", "-V"]), &["--version", "-V"]));
        assert!(!has_flag(&args(&["x", "--load", "a"]), &["--help", "-h"]));
        // `--help=foo` must NOT count as the bare flag.
        assert!(!has_flag(&args(&["x", "--help=foo"]), &["--help", "-h"]));
    }
}
