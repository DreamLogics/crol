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
         [lib]\npath = \"lib.rs\"\n\n[package.metadata.crol]\nschema = \"fleet.crol\"\n\n\
         [workspace]\n",
    ),
    ("lib.rs", ""),
    (
        "fleet.crol",
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
        dir.join("project.crol"),
        "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), \
         schema: Some(\"crate:fleet\"), .. }",
    )
    .unwrap();
    let output = crol(
        &dir,
        &["check", "--offline", "--project", "project.crol", "./data"],
    );
    assert!(schema_error(&output), "{output:?}");
}

#[test]
fn a_layer_command_runs_for_a_project_given_by_its_bare_file_name() {
    let dir = tree(
        "command-project",
        &[
            (
                "project.crol",
                "Project { layers: Command([\"cat\", \"layers.crol\"]), .. }",
            ),
            ("layers.crol", "[{ name: \"base\", dir: \"data\", .. }]"),
            ("data/a.crod", "+a: 1\n"),
        ],
    );
    let output = crol(&dir, &["eval", "--project", "project.crol"]);
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

// ---- Messages ----

#[test]
fn a_missing_step_of_a_path_says_what_is_missing() {
    let dir = tree("missing-step", &[("a.crod", "+web: {}\nweb.nope.x: 1\n")]);
    let output = crol(&dir, &["check", "a.crod"]);
    assert!(failed(&output), "{output:?}");
    let expected = "web.nope.x: `web.nope` is not set, so it cannot be patched";
    assert!(stderr(&output).contains(expected), "{output:?}");
}

#[test]
fn errors_in_a_project_file_point_at_the_file() {
    for (text, place) in [
        // A value that deserializing refuses.
        (
            "Project {\n    layers: List([{ name: 1, dir: \"data\", .. }]),\n    ..\n}\n",
            ":2:",
        ),
        // A syntax error.
        ("Project {\n    layers: ,\n}\n", ":2:"),
    ] {
        let dir = tree("project-errors", &[("project.crol", text)]);
        let output = crol(&dir, &["check"]);
        assert!(failed(&output), "{output:?}");
        let message = stderr(&output);
        assert!(message.contains("project.crol"), "{message}");
        assert!(message.contains(place), "{message}");
        assert!(!message.contains("<string>"), "{message}");
    }
}

#[test]
fn errors_in_a_schema_file_point_at_the_file() {
    let dir = tree(
        "schema-file-errors",
        &[
            ("s.crol", "Schema {\n    format: \"one\",\n}\n"),
            ("a.crod", "+a: 1\n"),
        ],
    );
    let output = crol(&dir, &["check", "a.crod", "--schema", "s.crol"]);
    assert!(failed(&output), "{output:?}");
    let message = stderr(&output);
    assert!(message.contains("s.crol"), "{message}");
    assert!(!message.contains("<string>"), "{message}");
}

#[test]
fn a_key_after_an_operator_points_at_the_missing_comma() {
    let dir = tree("missing-comma", &[("a.crod", "+a: 1 +b: 2\n")]);
    let output = crol(&dir, &["check", "a.crod"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("is a `,` missing"), "{output:?}");
}

// ---- Exit status (`SPEC.md` 11) ----

const SERVICE_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Services",
    types: {
        "app::Services": Transparent {
            name: "Services",
            source: ("src/lib.rs", 2),
            inner: Map(String, Named("app::Service")),
            ..
        },
        "app::Service": Struct {
            name: "Service",
            source: ("src/lib.rs", 1),
            fields: { replicas: { ty: U32, .. } },
            ..
        },
    },
}"#;

fn schema_tree(name: &str) -> PathBuf {
    tree(
        name,
        &[
            ("schema.crol", SERVICE_SCHEMA),
            ("good.crod", "+web: Service { replicas: 2 }\n"),
            ("bad.crod", "+web: Service { replicas: -2 }\n"),
            ("good.crol", "Service { replicas: 2 }\n"),
            ("bad.crol", "Service { replicas: -2 }\n"),
            ("other.crol", SERVICE_SCHEMA),
            (
                "warned/a.crod",
                "#![schema(super::other)]\n\n+web: Service { replicas: 1 }\n",
            ),
            ("warned/b.crod", "web.replicas: 3\n"),
        ],
    )
}

#[test]
fn every_command_fails_on_schema_errors_and_prints_no_data() {
    let dir = schema_tree("schema-errors");
    for file in ["bad.crod", "bad.crol"] {
        for command in ["check", "eval", "fingerprint"] {
            let args = [command, file, "--schema", "schema.crol"];
            let output = crol(&dir, &args);
            assert!(failed(&output), "{args:?}: {output:?}");
            assert_eq!(stdout(&output), "", "{args:?}");
            assert!(stderr(&output).contains("-2"), "{args:?}: {output:?}");

            let mut json = vec!["--message-format", "json"];
            json.extend(args);
            let output = crol(&dir, &json);
            assert!(failed(&output), "{args:?}: {output:?}");
            // The diagnostic, and nothing else.
            let text = stdout(&output);
            assert_eq!(text.lines().count(), 1, "{args:?}: {text}");
            assert!(text.starts_with("{\"severity\":\"error\""), "{text}");
        }
    }
    let output = crol(
        &dir,
        &["eval", "bad.crod", "web", "--schema", "schema.crol"],
    );
    assert!(failed(&output), "{output:?}");
    assert_eq!(stdout(&output), "");
    let args = ["eval", "--all-contexts", "bad.crod"];
    let output = crol(&dir, &[&args[..], &["--schema", "schema.crol"]].concat());
    assert!(failed(&output), "{output:?}");
    assert_eq!(stdout(&output), "");
}

#[test]
fn every_command_passes_data_the_schema_accepts() {
    let dir = schema_tree("schema-good");
    for file in ["good.crod", "good.crol"] {
        for command in ["check", "eval", "fingerprint"] {
            let output = crol(&dir, &[command, file, "--schema", "schema.crol"]);
            assert!(output.status.success(), "{command} {file}: {output:?}");
        }
    }
}

#[test]
fn warnings_do_not_fail_a_command() {
    let dir = schema_tree("warnings");
    let args = |command| [command, "./warned", "--schema", "schema.crol"];
    for command in ["check", "eval", "fingerprint"] {
        let output = crol(&dir, &args(command));
        assert!(output.status.success(), "{command}: {output:?}");
        assert!(stderr(&output).contains("another schema"), "{output:?}");
    }
    let output = crol(&dir, &args("eval"));
    assert!(stdout(&output).contains("replicas: 3"), "{output:?}");
}

/// Type names where serde gives the loader none (`SPEC.md` 10.2): a
/// transparent struct around a map has its name, and a map has none.
#[test]
fn check_reports_type_names_on_maps() {
    let schema = r#"Schema {
        format: 1,
        root: "app::App",
        types: {
            "app::App": Struct {
                name: "App",
                source: ("src/lib.rs", 1),
                fields: {
                    screen: { ty: Named("app::Screen"), .. },
                    stats: { ty: Map(String, U32), .. },
                },
                ..
            },
            "app::Screen": Transparent {
                name: "Screen",
                source: ("src/lib.rs", 2),
                inner: Map(String, U32),
                ..
            },
        },
    }"#;
    let dir = tree(
        "type-names-on-maps",
        &[
            ("schema.crol", schema),
            ("good.crod", "screen: Screen { a: 1 }\nstats: { a: 1 }\n"),
            ("bad.crod", "screen: Scren { a: 1 }\nstats: { a: 1 }\n"),
            (
                "warned.crod",
                "screen: Screen { a: 1 }\nstats: Stats { a: 1 }\n",
            ),
        ],
    );
    let check = |file| crol(&dir, &["check", file, "--schema", "schema.crol"]);
    let output = check("good.crod");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stderr(&output), "");
    let output = check("bad.crod");
    assert!(failed(&output), "{output:?}");
    assert!(
        stderr(&output).contains("error: `Scren { ... }` is not a `Screen`"),
        "{output:?}"
    );
    let output = check("warned.crod");
    assert!(output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains("warning: the type name `Stats` names nothing"),
        "{output:?}"
    );
}

// ---- Layers and keys (`SPEC.md` 11) ----

const PROJECT: &str = "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }";

fn project(name: &str) -> PathBuf {
    tree(
        name,
        &[
            ("project.crol", PROJECT),
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
    assert!(stderr(&output).contains("no project.crol"), "{output:?}");
    assert!(stderr(&output).contains("`./base`"), "{output:?}");
    let output = crol(&dir, &["eval", "./base"]);
    assert_eq!(stdout(&output).trim(), "a: 1", "{output:?}");
}

#[test]
fn a_missing_key_that_is_also_a_path_says_how_to_name_the_layer() {
    let dir = project("key-or-directory");
    let output = crol(&dir, &["eval", "data"]);
    assert!(failed(&output), "{output:?}");
    assert!(stderr(&output).contains("`./data`"), "{output:?}");
    let output = crol(&dir, &["eval", "nope"]);
    assert!(failed(&output), "{output:?}");
    assert!(!stderr(&output).contains("./nope"), "{output:?}");
}

#[test]
fn a_key_of_a_value_document() {
    let dir = project("value-key");
    std::fs::write(dir.join("v.crol"), "web: { port: 80 }\n..\n").unwrap();
    let output = crol(&dir, &["eval", "v.crol", "web.port"]);
    assert_eq!(stdout(&output).trim(), "80", "{output:?}");
    let output = crol(&dir, &["eval", "v.crol"]);
    assert!(stdout(&output).contains("web:"), "{output:?}");
    // Among other sources it is a layer, wherever the key is.
    let output = crol(&dir, &["eval", "./data", "v.crol", "web.port"]);
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
    for argument in ["./data", "data/a.crod", "base=data", "a.crol"] {
        let output = crol(&dir, &["explain", argument]);
        assert!(failed(&output), "{argument}: {output:?}");
        assert!(stderr(&output).contains("no key"), "{argument}: {output:?}");
    }
}

#[test]
fn check_reports_what_does_not_fit_a_type_attribute() {
    let dir = tree(
        "typed-binds",
        &[
            ("schema.crol", SERVICE_SCHEMA),
            (
                "good.crod",
                "#[type(Service)]\nbase := { replicas: 2 }\n+web: Service { ..&base }\n",
            ),
            (
                "bad.crod",
                "#[type(Service)]\nbase := { replicsa: 2 }\n+web: Service { replicas: 1 }\n",
            ),
            (
                "bad.crol",
                "#![type(Service)]\n#[type(Service.nope)]\nx := 1\nreplicas: 2\n",
            ),
        ],
    );
    let output = crol(&dir, &["check", "good.crod", "--schema", "schema.crol"]);
    assert!(output.status.success(), "{output:?}");
    let output = crol(&dir, &["check", "bad.crod", "--schema", "schema.crol"]);
    assert!(failed(&output), "{output:?}");
    assert!(
        stderr(&output).contains("`Service` has no field `replicsa`"),
        "{output:?}"
    );
    let output = crol(&dir, &["check", "bad.crol", "--schema", "schema.crol"]);
    assert!(failed(&output), "{output:?}");
    assert!(
        stderr(&output).contains("`Service` has no field `nope`"),
        "{output:?}"
    );
    assert_eq!(stderr(&output).matches("error").count(), 1, "{output:?}");
    // Without a schema, the attribute changes nothing.
    let output = crol(&dir, &["check", "bad.crod"]);
    assert!(output.status.success(), "{output:?}");
}
