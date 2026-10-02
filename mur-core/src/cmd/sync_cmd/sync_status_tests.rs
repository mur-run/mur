use super::*;

#[test]
fn run_status_works_on_fresh_system() {
    // run_status uses default_location() which creates dirs under $HOME/.mur/.
    // We just verify it doesn't panic and returns Ok.
    run_status().unwrap();
}
