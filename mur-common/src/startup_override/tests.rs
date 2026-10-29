use super::*;

const ISSUED: [IssuedBy; 2] = [IssuedBy::Cli, IssuedBy::Hub];
const CONFIRMED: [ConfirmedBy; 3] = [
    ConfirmedBy::Tty,
    ConfirmedBy::FlagConfirm,
    ConfirmedBy::HubNativeDialog,
];

#[test]
fn is_valid_pair_accepts_exactly_the_table() {
    let mut accepted = Vec::new();
    for issued in ISSUED {
        for confirmed in CONFIRMED {
            if is_valid_pair(issued, confirmed) {
                accepted.push((issued, confirmed));
            }
        }
    }
    assert_eq!(
        accepted,
        vec![
            (IssuedBy::Cli, ConfirmedBy::Tty),
            (IssuedBy::Cli, ConfirmedBy::FlagConfirm),
            (IssuedBy::Hub, ConfirmedBy::HubNativeDialog),
        ]
    );
}

#[test]
fn enums_round_trip_their_exact_strings() {
    let issued = [(IssuedBy::Cli, "\"cli\""), (IssuedBy::Hub, "\"hub\"")];
    for (value, wire) in issued {
        assert_eq!(serde_json::to_string(&value).unwrap(), wire);
        assert_eq!(serde_json::from_str::<IssuedBy>(wire).unwrap(), value);
    }
    let confirmed = [
        (ConfirmedBy::Tty, "\"tty\""),
        (ConfirmedBy::FlagConfirm, "\"flag:--confirm\""),
        (ConfirmedBy::HubNativeDialog, "\"hub:native-dialog\""),
    ];
    for (value, wire) in confirmed {
        assert_eq!(serde_json::to_string(&value).unwrap(), wire);
        assert_eq!(serde_json::from_str::<ConfirmedBy>(wire).unwrap(), value);
    }
}

#[test]
fn unknown_confirmed_by_fails_to_deserialize() {
    assert!(serde_json::from_str::<ConfirmedBy>("\"mouse\"").is_err());
    assert!(serde_json::from_str::<IssuedBy>("\"mouse\"").is_err());
}
