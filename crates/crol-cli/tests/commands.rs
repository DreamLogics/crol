//! The commands, run as a process: what they print, and how they exit.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn crol(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crol"))
        .args(["--color", "never"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("crol runs")
}

fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cli-commands")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Whether it failed as a command does: exit 1, and no panic (which exits
/// 101).
fn failed(output: &Output) -> bool {
    output.status.code() == Some(1)
}

// ---- Diagnostics (`SPEC.md` 7.5, 11) ----

#[test]
fn a_cycle_between_root_keys_is_reported() {
    let dir = tree("root-cycle", &[("c.crod", "+a: &root.b\n+b: &root.a\n")]);
    for command in ["check", "eval", "fingerprint"] {
        let output = crol(&dir, &[command, "c.crod"]);
        assert!(failed(&output), "{command}: {output:?}");
        let text = stderr(&output);
        assert!(text.contains("depends on itself"), "{command}: {text}");
        assert!(text.contains("c.crod:"), "{command}: {text}");
        // Every reference in it, with its span (`SPEC.md` 7.5).
        assert!(text.contains("&root.a"), "{command}: {text}");
        assert!(text.contains("&root.b"), "{command}: {text}");
    }
}

#[test]
fn a_cycle_between_root_keys_is_reported_as_json() {
    let dir = tree(
        "root-cycle-json",
        &[("c.crod", "+a: &root.b\n+b: &root.a\n")],
    );
    for command in ["check", "eval", "fingerprint"] {
        let output = crol(&dir, &["--message-format", "json", command, "c.crod"]);
        assert!(failed(&output), "{command}: {output:?}");
        let text = stdout(&output);
        assert!(text.contains("depends on itself"), "{command}: {text}");
        assert!(text.contains("\"file\":\"c.crod\""), "{command}: {text}");
        assert_eq!(text.lines().count(), 1, "{command}: {text}");
    }
}

#[test]
fn a_root_key_that_holds_the_root_is_reported() {
    let dir = tree("root-holds-root", &[("c.crod", "+a: &root\n")]);
    for format in ["human", "json"] {
        let output = crol(&dir, &["--message-format", format, "check", "c.crod"]);
        assert!(failed(&output), "{format}: {output:?}");
        let text = stdout(&output) + &stderr(&output);
        assert!(text.contains("contains itself"), "{format}: {text}");
        assert!(text.contains("c.crod"), "{format}: {text}");
    }
}

#[test]
fn a_cycle_in_a_block_is_still_reported() {
    let dir = tree(
        "block-cycle",
        &[("c.crod", "+t: { x: &self.y, y: &self.x }\n")],
    );
    let output = crol(&dir, &["check", "c.crod"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("depends on itself"), "{output:?}");
}

#[test]
fn a_diagnostic_about_no_place_has_no_file() {
    let dir = tree("no-place", &[("c.crod", "+a: 1\n")]);
    // A path that is not one is about the argument, not about a file.
    let output = crol(&dir, &["explain", "c.crod", "a b"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("`a b` is not"), "{output:?}");

    let output = crol(
        &dir,
        &["--message-format", "json", "explain", "c.crod", "a b"],
    );
    assert!(failed(&output), "{output:?}");
    assert!(stdout(&output).contains("`a b` is not"), "{output:?}");
    assert!(stdout(&output).contains("\"file\":null"), "{output:?}");
}
