use mur_agent_runtime::durable::ledger::Ledger;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(tag = "event")]
enum E {
    A { x: u32 },
    B { s: String },
}

#[test]
fn append_and_scan_round_trip() {
    let dir = TempDir::new().unwrap();
    let mut led = Ledger::open(dir.path()).unwrap();
    led.append(&E::A { x: 1 }).unwrap();
    led.append(&E::B { s: "hi".into() }).unwrap();
    led.flush().unwrap();

    let evs: Vec<E> = Ledger::scan_days::<E>(dir.path(), 7)
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(evs, vec![E::A { x: 1 }, E::B { s: "hi".into() }]);
}

#[test]
fn corrupt_last_line_skipped() {
    let dir = TempDir::new().unwrap();
    let today = chrono::Local::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let path = dir.path().join(format!("{today}.jsonl"));
    std::fs::write(
        &path,
        b"{\"event\":\"A\",\"x\":1}\n{not json}\n{\"event\":\"B\",\"s\":\"k\"}\n",
    )
    .unwrap();
    let evs: Vec<E> = Ledger::scan_days::<E>(dir.path(), 1)
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(evs, vec![E::A { x: 1 }, E::B { s: "k".into() }]);
}

#[test]
fn empty_dir_returns_empty_iter() {
    let dir = TempDir::new().unwrap();
    let evs: Vec<E> = Ledger::scan_days::<E>(dir.path(), 7)
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert!(evs.is_empty());
}

#[test]
fn missing_dir_does_not_error() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("never_created");
    let evs: Vec<E> = Ledger::scan_days::<E>(&nonexistent, 7)
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert!(evs.is_empty());
}
