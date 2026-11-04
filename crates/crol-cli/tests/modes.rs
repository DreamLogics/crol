//! The extension tells the command line a file's mode (`SPEC.md` 4, 11).

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
        .join("cli-modes")
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

const PROJECT: &str = "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }";

/// `#![type]`, a top-level `..` and a typed block are what data mode rejects.
const VALUE_DOCUMENTS: &[(&str, &str)] = &[
    ("typed.crov", "#![type(Config)]\n\nport: 80\n"),
    ("marker.crov", "port: 80\n..\n"),
    ("block.crov", "Config { port: 80 }\n"),
    ("expression.crov", "40 + 2\n"),
];

#[test]
fn check_reads_a_crov_file_in_value_mode() {
    let dir = tree("check-value", VALUE_DOCUMENTS);
    for (name, _) in VALUE_DOCUMENTS {
        let output = crol(&dir, &["check", name]);
        assert!(output.status.success(), "{name}: {output:?}");
    }
    std::fs::write(dir.join("broken.crov"), "a: 1 / 0\n").unwrap();
    let output = crol(&dir, &["check", "broken.crov"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("zero"), "{output:?}");
}

#[test]
fn value_mode_errors_are_value_mode_errors() {
    let dir = tree("check-variants", &[("a.crov", "<prod> { a: 1 }\n")]);
    let output = crol(&dir, &["check", "a.crov"]);
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn eval_prints_a_value_document() {
    let dir = tree(
        "eval-value",
        &[
            ("number.crov", "40 + 2\n"),
            (
                "typed.crov",
                "#![type(Config)]\n\nport: 80\nname: \"web\"\n",
            ),
        ],
    );
    let output = crol(&dir, &["eval", "number.crov"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), "42\n");
    let output = crol(&dir, &["eval", "typed.crov"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).contains("Config {"), "{output:?}");

    // One value of it, as of layers.
    let output = crol(&dir, &["eval", "typed.crov", "name"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), "\"web\"\n");
    let output = crol(&dir, &["eval", "typed.crov", "missing"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("no value at"), "{output:?}");
}

#[test]
fn a_value_document_has_no_contexts() {
    let dir = tree("no-contexts", &[("a.crov", "1\n")]);
    for args in [
        &["eval", "a.crov", "--context", "prod"][..],
        &["eval", "a.crov", "--all-contexts"],
        &["check", "a.crov", "--discrete", "cache"],
    ] {
        let output = crol(&dir, args);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr(&output).contains("no variants"), "{output:?}");
    }
}

#[test]
fn fingerprint_reads_a_crov_file_in_value_mode() {
    let dir = tree(
        "fingerprint",
        &[
            ("a.crov", "{ a: 1, b: [2] }\n"),
            ("b.crov", "a: 1\nb: [1 + 1]\n"),
            ("c.crov", "a: 2\n"),
            ("number.crov", "1\n"),
            ("other.crov", "2\n"),
        ],
    );
    let print = |name: &str| {
        let output = crol(&dir, &["fingerprint", name]);
        assert!(output.status.success(), "{output:?}");
        stdout(&output)
    };
    assert_eq!(print("a.crov"), print("b.crov"));
    assert_ne!(print("a.crov"), print("c.crov"));
    // Not only keyed values have one.
    assert_ne!(print("number.crov"), print("other.crov"));
}

#[test]
fn explain_says_a_value_has_no_layers() {
    let dir = tree("explain", &[("a.crov", "port: 80\n")]);
    let output = crol(&dir, &["explain", "a.crov", "port"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no layers to explain"),
        "{output:?}"
    );
}

#[test]
fn a_crod_file_is_read_in_data_mode() {
    let dir = tree(
        "data",
        &[
            ("a.crod", "+port: 80\n"),
            ("typed.crod", "#![type(Config)]\n\n+port: 80\n"),
        ],
    );
    assert!(crol(&dir, &["check", "a.crod"]).status.success());
    let output = crol(&dir, &["check", "typed.crod"]);
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn a_crov_file_among_layers_is_an_error() {
    let dir = tree("among", &[("a.crod", "+port: 80\n"), ("b.crov", "1\n")]);
    for args in [
        &["check", "a.crod", "b.crov"][..],
        &["check", "b.crov", "a.crod"],
        &["check", "base=b.crov"],
    ] {
        let output = crol(&dir, args);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr(&output).contains("cannot be a layer"), "{output:?}");
    }
}

#[test]
fn a_crov_file_in_a_layer_directory_fails_loudly() {
    let dir = tree(
        "layer",
        &[
            ("project.crov", PROJECT),
            ("data/a.crod", "+a: 1\n"),
            ("data/stray.crov", "1\n"),
        ],
    );
    for args in [
        &["check"][..],
        &["check", "data"],
        &["eval"],
        &["fingerprint"],
    ] {
        let output = crol(&dir, args);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr(&output).contains("stray.crov"), "{output:?}");
        assert!(stderr(&output).contains("cannot be a layer"), "{output:?}");
    }
}

#[test]
fn the_project_file_may_lie_in_a_layer_directory() {
    let dir = tree(
        "project-in-layer",
        &[
            ("project.crov", PROJECT),
            ("data/a.crod", "+a: 1\n"),
            ("b.crod", "+b: 2\n"),
        ],
    );
    let output = crol(&dir, &["eval", "."]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output), "b: 2\na: 1\n");

    // Any other value document there is not excused.
    std::fs::write(dir.join("schema.crov"), "1\n").unwrap();
    let output = crol(&dir, &["eval", "."]);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr(&output).contains("schema.crov"), "{output:?}");
}

#[test]
fn a_crol_file_names_the_extension_to_use() {
    let dir = tree(
        "retired",
        &[
            ("project.crov", PROJECT),
            ("data/a.crod", "+a: 1\n"),
            ("data/old.crol", "+b: 2\n"),
            ("direct.crol", "+c: 3\n"),
        ],
    );
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("rename it to `old.crod`"),
        "{output:?}"
    );

    let output = crol(&dir, &["check", "direct.crol"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("rename it to `direct.crod`"),
        "{output:?}"
    );
}

#[test]
fn a_leftover_project_crol_is_not_no_project() {
    let dir = tree(
        "leftover",
        &[("project.crol", PROJECT), ("data/a.crod", "+a: 1\n")],
    );
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("rename it to `project.crov`"), "{message}");
    assert!(!message.contains("no layers given"), "{message}");

    let output = crol(&dir.join("data"), &["check"]);
    assert!(
        stderr(&output).contains("rename it to `project.crov`"),
        "{output:?}"
    );
}

#[test]
fn the_project_and_schema_options_read_value_documents() {
    let dir = tree(
        "options",
        &[
            ("data/a.crod", "+a: 1\n"),
            ("p.crod", PROJECT),
            ("s.crod", ""),
        ],
    );
    let output = crol(&dir, &["check", "--project", "p.crod"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("cannot be read as a value document"),
        "{output:?}"
    );
    let output = crol(&dir, &["check", "data", "--schema", "s.crod"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("cannot be read as a value document"),
        "{output:?}"
    );
}

#[test]
fn a_project_file_can_be_checked_as_a_value_document() {
    let dir = tree(
        "check-project",
        &[
            ("project.crov", PROJECT),
            ("wrong/project.crov", "Project { layers: 1, .. }\n"),
        ],
    );
    assert!(crol(&dir, &["check", "project.crov"]).status.success());
    let output = crol(&dir, &["check", "wrong/project.crov"]);
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn a_value_document_is_checked_against_its_schema() {
    let schema = crol::schema::build::<crol::project::Project>()
        .to_text()
        .unwrap();
    let dir = tree(
        "value-schema",
        &[
            ("schema.crov", &schema),
            ("good.crov", &format!("#![schema(schema)]\n\n{PROJECT}\n")),
            (
                "bad.crov",
                "#![schema(schema)]\n\nProject { layers: 1, .. }\n",
            ),
            ("given.crov", "Project { layers: 1, .. }\n"),
        ],
    );
    assert!(crol(&dir, &["check", "good.crov"]).status.success());
    assert!(!crol(&dir, &["check", "bad.crov"]).status.success());
    assert!(crol(&dir, &["check", "given.crov"]).status.success());
    let args = ["check", "given.crov", "--schema", "schema.crov"];
    assert!(!crol(&dir, &args).status.success());

    // With the schema, fields that take their defaults are shown.
    let output = crol(&dir, &["eval", "good.crov"]);
    assert!(stdout(&output).contains("// default"), "{output:?}");
    let output = crol(&dir, &["eval", "good.crov", "--no-defaults"]);
    assert!(!stdout(&output).contains("// default"), "{output:?}");
}

#[test]
fn fmt_takes_both_extensions_and_refuses_crol() {
    let dir = tree(
        "fmt",
        &[
            ("a.crod", "+a:1,\n"),
            ("b.crov", "#![type(Config)]\n\nport:80\n"),
            ("c.crol", "a:1\n"),
            ("d.txt", "a:1\n"),
        ],
    );
    let output = crol(&dir, &["fmt", "a.crod", "b.crov", "d.txt"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("a.crod")).unwrap(),
        "+a: 1\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("b.crov")).unwrap(),
        "#![type(Config)]\n\nport: 80\n"
    );

    let output = crol(&dir, &["fmt", "c.crol"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("`.crod`"), "{output:?}");
    assert!(stderr(&output).contains("`.crov`"), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("c.crol")).unwrap(),
        "a:1\n"
    );

    // It takes files; a directory is refused rather than walked.
    let output = crol(&dir, &["fmt", "."]);
    assert!(!output.status.success(), "{output:?}");
}

#[test]
fn from_ron_writes_a_value_document() {
    let dir = tree("from-ron", &[("a.ron", "Config(port: 80)\n")]);
    let output = crol(&dir, &["from-ron", "a.ron"]);
    assert!(output.status.success(), "{output:?}");
    std::fs::write(dir.join("a.crov"), &output.stdout).unwrap();
    assert!(crol(&dir, &["check", "a.crov"]).status.success());
}

#[test]
fn init_refuses_a_project_crol() {
    let dir = tree(
        "init",
        &[
            ("Cargo.toml", "[package]\nname = \"app\"\n"),
            ("project.crol", PROJECT),
        ],
    );
    let output = crol(&dir, &["--init"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("rename it to `project.crov`"),
        "{output:?}"
    );
    assert!(!dir.join("project.crov").exists());
}

const DEFAULTS_SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Service",
    types: {
        "app::Service": Struct {
            name: "Service",
            source: ("src/lib.rs", 1),
            fields: { replicas: { ty: U32, default: Value(3), .. } },
            ..
        },
        "app::Config": Struct {
            name: "Config",
            source: ("src/lib.rs", 2),
            fields: { port: { ty: U16, .. }, host: { ty: String, default: Value("h"), .. } },
            ..
        },
    },
}"#;

#[test]
fn default_reads_the_schema_tools_have() {
    let data = "+web: Service { replicas: &default * 2, .. }\n";
    let dir = tree(
        "default",
        &[
            ("schema.crov", DEFAULTS_SCHEMA),
            ("given.crod", data),
            ("named.crod", &format!("#![schema(schema)]\n\n{data}")),
            (
                "project.crov",
                "Project { layers: List([{ name: \"d\", dir: \"data\", .. }]), schema: Some(\"schema\"), .. }",
            ),
            ("data/a.crod", data),
        ],
    );
    let args = ["eval", "given.crod", "--schema", "schema.crov"];
    let output = crol(&dir, &args);
    assert!(stdout(&output).contains("replicas: 6"), "{output:?}");
    let output = crol(&dir, &["eval", "named.crod"]);
    assert!(stdout(&output).contains("replicas: 6"), "{output:?}");
    let output = crol(&dir, &["eval"]);
    assert!(stdout(&output).contains("replicas: 6"), "{output:?}");
    let output = crol(&dir, &["check"]);
    assert!(output.status.success(), "{output:?}");

    // With no schema at all, it stays an error.
    let output = crol(&dir, &["check", "given.crod"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr(&output).contains("no schema is given"), "{output:?}");
}

const FILES_PROJECT: &str = r#"Project {
    layers: List([{ name: "d", dir: "data", .. }]),
    schema: Some("schema"),
    files: [
        { glob: "config::special", type: "app::Service" },
        { glob: "config::**::*", type: "Config" },
    ],
    ..
}"#;
