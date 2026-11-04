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

// ---- Bare file names (`SPEC.md` 4.1, 4.2) ----

/// A Cargo package that publishes a schema, with a data file in it.
const FLEET: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"fleet\"\nversion = \"1.2.0\"\nedition = \"2021\"\n\n\
         [lib]\npath = \"lib.rs\"\n\n[package.metadata.crol]\nschema = \"fleet.crov\"\n\n\
         [workspace]\n",
    ),
    ("lib.rs", ""),
    (
        "fleet.crov",
        r#"Schema {
            format: 1,
            root: "app::Service",
            types: {
                "app::Service": Struct {
                    name: "Service",
                    source: ("lib.rs", 1),
                    fields: { replicas: { ty: U32, .. } },
                    ..
                },
            },
        }"#,
    ),
    (
        "x.crod",
        "#![schema(\"crate:fleet\")]\n+web: Service { replicas: -2 }\n",
    ),
    (
        "sub/x.crod",
        "#![schema(\"crate:fleet\")]\n+web: Service { replicas: -2 }\n",
    ),
];

/// The schema was found: what is wrong is the data.
fn schema_error(output: &Output) -> bool {
    failed(output) && stderr(output).contains("-2") && !stderr(output).contains("not cached")
}

#[test]
fn a_crate_schema_is_found_for_a_bare_file_name() {
    let dir = tree("crate-bare", FLEET);
    for name in ["x.crod", "./x.crod", "sub/x.crod", "./sub/x.crod"] {
        let output = crol(&dir, &["check", "--offline", name]);
        assert!(schema_error(&output), "{name}: {output:?}");
    }
}

#[test]
fn a_crate_schema_is_found_from_a_directory_below_the_package() {
    let dir = tree("crate-below", FLEET);
    for name in ["x.crod", "./x.crod"] {
        let output = crol(&dir.join("sub"), &["check", "--offline", name]);
        assert!(schema_error(&output), "{name}: {output:?}");
    }
    let output = crol(&dir.join("sub"), &["check", "--offline", "../x.crod"]);
    assert!(schema_error(&output), "{output:?}");
}

#[test]
fn a_crate_schema_of_a_project_given_by_its_bare_file_name() {
    let dir = tree("crate-project", FLEET);
    std::fs::create_dir(dir.join("data")).unwrap();
    std::fs::write(dir.join("data/a.crod"), "+web: Service { replicas: -2 }\n").unwrap();
    std::fs::write(
        dir.join("project.crov"),
        "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), \
         schema: Some(\"crate:fleet\"), .. }",
    )
    .unwrap();
    let output = crol(
        &dir,
        &["check", "--offline", "--project", "project.crov", "./data"],
    );
    assert!(schema_error(&output), "{output:?}");
}

#[test]
fn a_layer_command_runs_for_a_project_given_by_its_bare_file_name() {
    let dir = tree(
        "command-project",
        &[
            (
                "project.crov",
                "Project { layers: Command([\"cat\", \"layers.crov\"]), .. }",
            ),
            ("layers.crov", "[{ name: \"base\", dir: \"data\", .. }]"),
            ("data/a.crod", "+a: 1\n"),
        ],
    );
    let output = crol(&dir, &["eval", "--project", "project.crov"]);
    assert_eq!(stdout(&output).trim(), "a: 1", "{output:?}");
}

#[test]
fn super_works_from_a_relative_path() {
    let dir = tree(
        "super-relative",
        &[
            ("y.crod", "+y: 1\n"),
            ("sub/z.crod", "#![extends(super::y)]\n+z: 1\n"),
        ],
    );
    for name in ["sub/z.crod", "./sub/z.crod"] {
        let output = crol(&dir, &["eval", name]);
        assert_eq!(stdout(&output), "y: 1\nz: 1\n", "{name}: {output:?}");
    }
    // Above the root, here the working directory, is still an error.
    let output = crol(&dir.join("sub"), &["eval", "z.crod"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("above the root"), "{output:?}");
    let output = crol(&dir.join("sub"), &["eval", "./z.crod"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("above the root"), "{output:?}");
}

#[test]
fn an_extends_that_fails_is_reported() {
    let dir = tree(
        "extends-errors",
        &[
            ("missing.crod", "#![extends(nothing)]\n+a: 1\n"),
            ("loop_a.crod", "#![extends(loop_b)]\n+a: 1\n"),
            ("loop_b.crod", "#![extends(loop_a)]\n+b: 1\n"),
        ],
    );
    for command in ["check", "eval", "fingerprint"] {
        let output = crol(&dir, &[command, "missing.crod"]);
        assert!(failed(&output), "{command}: {output:?}");
        assert!(stderr(&output).contains("names nothing"), "{output:?}");
        let output = crol(&dir, &[command, "loop_a.crod"]);
        assert!(failed(&output), "{command}: {output:?}");
        assert!(stderr(&output).contains("cycle"), "{output:?}");
    }
}

// ---- Layers and keys (`SPEC.md` 11) ----

const PROJECT: &str = "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }";

fn project(name: &str) -> PathBuf {
    tree(
        name,
        &[
            ("project.crov", PROJECT),
            ("data/a.crod", "+web: { port: 80, tags: [\"x\"] }\n"),
        ],
    )
}

#[test]
fn a_lone_key_reads_the_project() {
    let dir = project("lone-key");
    for (key, value) in [("web.port", "80"), ("web.tags[0]", "\"x\"")] {
        let output = crol(&dir, &["eval", key]);
        assert!(output.status.success(), "{key}: {output:?}");
        assert_eq!(stdout(&output).trim(), value);
    }
    // A key alone, not only a path: this directory is no matter.
    std::fs::create_dir(dir.join("web")).unwrap();
    let output = crol(&dir, &["eval", "web"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).contains("port: 80"), "{output:?}");
}

#[test]
fn a_key_does_not_depend_on_what_exists() {
    let dir = project("key-not-a-file");
    std::fs::write(dir.join("web.port"), "not a layer").unwrap();
    let output = crol(&dir, &["eval", "web.port"]);
    assert_eq!(stdout(&output).trim(), "80", "{output:?}");
    // Before the last argument, a bare name is a layer.
    let output = crol(&dir, &["eval", "data", "web.port"]);
    assert_eq!(stdout(&output).trim(), "80", "{output:?}");
}

#[test]
fn what_cannot_be_a_key_is_a_layer() {
    let dir = project("layer-forms");
    for arguments in [
        &["./data"][..],
        &["data/"],
        &["::data"],
        &["base=data"],
        &["data/a.crod"],
    ] {
        let mut args = vec!["eval"];
        args.extend(arguments);
        let output = crol(&dir, &args);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        assert!(stdout(&output).contains("web:"), "{arguments:?}");
    }
    std::fs::write(dir.join("b.crod"), "web.port: 81\n").unwrap();
    let output = crol(&dir, &["eval", "::data", "b.crod"]);
    assert!(stdout(&output).contains("port: 81"), "{output:?}");
    let output = crol(&dir, &["eval", "::data", "b.crod", "web.port"]);
    assert_eq!(stdout(&output).trim(), "81", "{output:?}");
}

#[test]
fn a_key_without_layers_says_how_to_name_a_layer() {
    let dir = tree("key-no-project", &[("base/a.crod", "+a: 1\n")]);
    let output = crol(&dir, &["eval", "base"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("no project.crov"), "{output:?}");
    assert!(stderr(&output).contains("`./base`"), "{output:?}");
    let output = crol(&dir, &["eval", "./base"]);
    assert_eq!(stdout(&output).trim(), "a: 1", "{output:?}");
}

#[test]
fn a_key_of_a_value_document() {
    let dir = project("value-key");
    std::fs::write(dir.join("v.crov"), "web: { port: 80 }\n..\n").unwrap();
    let output = crol(&dir, &["eval", "v.crov", "web.port"]);
    assert_eq!(stdout(&output).trim(), "80", "{output:?}");
    let output = crol(&dir, &["eval", "v.crov"]);
    assert!(stdout(&output).contains("web:"), "{output:?}");
    // Among other sources it is a layer, wherever the key is.
    let output = crol(&dir, &["eval", "./data", "v.crov", "web.port"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("cannot be a layer"), "{output:?}");
}

#[test]
fn explain_reads_the_project_for_a_lone_key() {
    let dir = project("explain-key");
    let output = crol(&dir, &["explain", "web.port"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).contains("definition"), "{output:?}");
    let output = crol(&dir, &["explain", "./data", "web.port"]);
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn explain_needs_a_key() {
    let dir = project("explain-no-key");
    for argument in ["./data", "data/a.crod", "base=data", "a.crov"] {
        let output = crol(&dir, &["explain", argument]);
        assert!(failed(&output), "{argument}: {output:?}");
        assert!(stderr(&output).contains("no key"), "{argument}: {output:?}");
    }
}
