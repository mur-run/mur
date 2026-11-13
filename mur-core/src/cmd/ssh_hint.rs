//! K5 (SSH setup journey): `mur ssh` is not a command, and clap's own
//! "unrecognized subcommand, did you mean `push`" sent people the wrong way.
//! Every backticked `mur …` here is parsed by `perm_hint_tests`, so only
//! runnable commands are backticked.

/// What `mur ssh …` prints, whatever followed it.
pub fn hint() -> String {
    "`mur ssh` is not a command. An agent's SSH grants live under mur agent ssh:\n\
     \x20 `mur agent ssh list <agent>`  show the keys and hosts it may sign for\n\
     \x20 `mur agent ssh allow <agent> --key <fingerprint> --host <host>`  let it sign for one host\n\
     \x20 `mur agent ssh revoke <agent> --key <fingerprint>`  take a grant back"
        .to_string()
}
