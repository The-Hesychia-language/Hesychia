//! `hsy test`: each `@test` function runs on its own, and a failure says why.

use std::path::Path;
use std::process::Command;

const HSY: &str = env!("CARGO_BIN_EXE_hsy");

#[test]
fn reports_each_test() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testing");
    let output =
        Command::new(HSY).arg("test").arg("checks.hsy").current_dir(&dir).output().expect("hsy runs");
    let out = String::from_utf8_lossy(&output.stdout);
    let expected = [
        "ok    parses_numbers",
        "FAIL  sorts  (checks.hsy)",
        "      panic: equal: [1, 2, 3] != [1, 3, 2]",
        "      at checks.hsy:20",
        "ok    doubles",
        "FAIL  words  (checks.hsy)",
        "      panic: equal: 'x' != 'y'",
        "      at checks.hsy:29",
        "FAIL  broken  (checks.hsy): it does not compile",
        "FAIL  reads_bad_number  (checks.hsy)",
        "      error: `ParseError` was raised and not caught",
        "      at checks.hsy:37",
        "FAIL  out_of_bounds  (checks.hsy)",
        "      before",
        "      panic: the program stopped on a fault, such as an index out of range",
        "      at checks.hsy:43",
        "7 tests, 5 failed",
    ];
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        expected,
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected `i32`, found `str`"));
}

#[test]
fn filters_by_name_and_leaves_tests_out_of_programs() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testing");
    let output =
        Command::new(HSY).args(["test", "checks.hsy", "doub"]).current_dir(&dir).output().expect("hsy runs");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok    doubles\n1 test, 0 failed\n");
    assert!(output.status.success());
    // `hsy run` builds the program without its tests, the broken one included.
    let output = Command::new(HSY).args(["run", "checks.hsy"]).current_dir(&dir).output().expect("hsy runs");
    // The program prints in Windows' text mode there, which ends lines with \r\n.
    assert_eq!(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"), "8\n");
}
