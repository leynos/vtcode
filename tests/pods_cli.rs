#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn pods_list_dispatches_through_the_binary() {
    let home = tempdir().expect("create temp home");

    let _assertion = assert_cmd::cargo::cargo_bin_cmd!("diapason")
        .env("HOME", home.path())
        .args(["pods", "list"])
        .assert()
        .failure()
        .stderr(contains("no active pod configured"));
}
