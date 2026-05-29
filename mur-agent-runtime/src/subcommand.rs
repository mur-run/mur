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
}
