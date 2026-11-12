//! The host step, pure: compare a scan with the user's own `known_hosts`
//! and pick which scanned keys a set of choices grants (SSH setup spec,
//! "Step 2: host", verdict table).

use anyhow::{Result, bail};

use super::super::resolve::ScannedKey;

/// What the user's `known_hosts` says about the scanned keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A scanned key equals a key the user stores for this host.
    Trusted,
    /// The user stores nothing for this host, or `downgraded`: a key
    /// matched, but the agent can write the user's `known_hosts`, so the
    /// match proves nothing.
    Unknown { downgraded: bool },
    /// The user stores keys for this host and none was scanned.
    Differs,
}

/// The verdict and the scanned fingerprints that equal a stored one.
pub fn verdict(
    scanned: &[ScannedKey],
    stored: &[String],
    agent_can_write_known_hosts: bool,
) -> (Verdict, Vec<String>) {
    if stored.is_empty() {
        return (Verdict::Unknown { downgraded: false }, Vec::new());
    }
    let matched: Vec<String> = scanned
        .iter()
        .filter(|k| stored.contains(&k.fingerprint))
        .map(|k| k.fingerprint.clone())
        .collect();
    match (matched.is_empty(), agent_can_write_known_hosts) {
        (true, _) => (Verdict::Differs, Vec::new()),
        (false, true) => (Verdict::Unknown { downgraded: true }, Vec::new()),
        (false, false) => (Verdict::Trusted, matched),
    }
}

/// How the user vouched for host keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostChoice {
    /// A fingerprint the operator published, pasted by the user.
    pub pasted: Option<String>,
    /// Scanned fingerprints the user ticked "I compared this" on.
    pub ticked: Vec<String>,
}

/// The scanned keys `choice` grants under `verdict`. Never more than the
/// table allows; an empty grant is an error, not a no-op.
pub fn select(
    host: &str,
    scanned: &[ScannedKey],
    verdict: &Verdict,
    matched: &[String],
    choice: &HostChoice,
) -> Result<Vec<ScannedKey>> {
    let pick = |fps: &[String]| -> Vec<ScannedKey> {
        scanned
            .iter()
            .filter(|k| fps.contains(&k.fingerprint))
            .cloned()
            .collect()
    };
    let pasted = match &choice.pasted {
        Some(fp) if !scanned.iter().any(|k| &k.fingerprint == fp) => {
            let offered: Vec<&str> = scanned.iter().map(|k| k.fingerprint.as_str()).collect();
            bail!(
                "{host} did not offer host key {fp}; it offered:\n  {}\n\
                 Nothing was changed. If the server rotated its key, verify the new one out \
                 of band before allowing it.",
                offered.join("\n  ")
            );
        }
        other => other.clone(),
    };
    let chosen = match verdict {
        Verdict::Trusted => matched.to_vec(),
        Verdict::Unknown { .. } => {
            let mut fps: Vec<String> = pasted.into_iter().collect();
            for t in &choice.ticked {
                if scanned.iter().any(|k| &k.fingerprint == t) && !fps.contains(t) {
                    fps.push(t.clone());
                }
            }
            fps
        }
        Verdict::Differs => {
            if !choice.ticked.is_empty() {
                bail!("{}", differs_text(host));
            }
            pasted.into_iter().collect()
        }
    };
    if chosen.is_empty() {
        match verdict {
            Verdict::Differs => bail!("{}", differs_text(host)),
            _ => bail!(
                "no host key of {host} was confirmed: paste the fingerprint the operator \
                 published (--host-key), or compare a scanned key yourself. Nothing was changed."
            ),
        }
    }
    Ok(pick(&chosen))
}

/// The refusal on `differs`, naming both exits.
pub fn differs_text(host: &str) -> String {
    format!(
        "your ~/.ssh/known_hosts stores a different key for {host}: the host was rebuilt, or \
         the connection is intercepted. Either pass the fingerprint the operator published \
         with --host-key, or update ~/.ssh/known_hosts yourself (`ssh-keygen -R {host}`) and \
         try again. Nothing was changed."
    )
}
