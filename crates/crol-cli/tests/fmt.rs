//! `crol fmt`, run as a process.

use std::path::PathBuf;
use std::process::{Command, Output};

fn crol(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crol"))
        .args(args)
        .output()
        .expect("crol runs")
}

/// A fresh file with `text` in the test's temporary directory.
fn file(name: &str, text: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fmt");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn formats_in_place() {
    let path = file("in_place.crol", "+web:{port:80}");
    let output = crol(&["fmt", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "+web: { port: 80 },\n"
    );
}

#[test]
fn check_reports_unformatted_files_without_changing_them() {
    let formatted = file("check_ok.crol", "a: 1,\n");
    let unformatted = file("check_bad.crol", "a:1");
    let output = crol(&["fmt", "--check", formatted.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");

    let output = crol(&["fmt", "--check", unformatted.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not formatted"));
    assert_eq!(std::fs::read_to_string(&unformatted).unwrap(), "a:1");
}

#[test]
fn syntax_errors_fail_and_leave_the_file_alone() {
    let path = file("syntax_error.crol", "a: 1 b: 2");
    let output = crol(&["fmt", "--color", "never", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("expected `,` between entries"), "{stderr}");
    assert!(stderr.contains("syntax_error.crol:1:5"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a: 1 b: 2");
}

#[test]
fn invalid_utf8_is_an_error() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fmt");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("not_utf8.crol");
    std::fs::write(&path, b"a: \"\xFF\"").unwrap();
    let output = crol(&["fmt", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not valid UTF-8"));
}
