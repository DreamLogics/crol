//! A struct as the root type, through the commands (`SPEC.md` 4.1, 11).

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

const SCHEMA: &str = r#"Schema {
    format: 1,
    root: "app::Config",
    types: {
        "app::Config": Struct {
            name: "Config",
            source: ("src/lib.rs", 1),
            fields: {
                name: { ty: String, .. },
                options: { ty: Named("app::Options"), .. },
                retries: { ty: U32, default: Value(3), .. },
                note: { ty: Option(String), default: Value(None), .. },
                level: { ty: U32, default: Unrecorded, .. },
            },
            ..
        },
        "app::Options": Struct {
            name: "Options",
            source: ("src/lib.rs", 2),
            fields: { a: { ty: Bool, .. } },
            ..
        },
        "app::Definition": Enum {
            name: "Definition",
            source: ("src/lib.rs", 3),
            variants: { Options: { shape: Newtype(Named("app::Options")), .. } },
            ..
        },
    },
}"#;

const PROJECT: &str = r#"Project {
    layers: List([
        { name: "base", dir: "base", .. },
        { name: "local", dir: "local", .. },
    ]),
    schema: Some("schema"),
    ..
}"#;

fn project(name: &str, base: &str, local: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cli-struct-root")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("base")).unwrap();
    std::fs::create_dir_all(dir.join("local")).unwrap();
    std::fs::write(dir.join("project.crov"), PROJECT).unwrap();
    std::fs::write(dir.join("schema.crov"), SCHEMA).unwrap();
    std::fs::write(dir.join("base/a.crod"), base).unwrap();
    std::fs::write(dir.join("local/a.crod"), local).unwrap();
    dir
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const COMPLETE: &str = "name: \"x\"\noptions: { a: true }\nretries: 1\nnote: None\nlevel: 2\n";

#[test]
fn check_takes_root_keys_as_fields_of_the_composed_root() {
    let dir = project("fields", "name: \"x\"\noptions: { a: true }\n", "..\n");
    let output = crol(&dir, &["check"]);
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn a_missing_field_is_reported_for_the_composed_root_on_the_top_document() {
    let dir = project("missing", "name: \"x\"\n", "retries: 1\n");
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success(), "{output:?}");
    let text = stderr(&output);
    assert!(
        text.contains("`Config` is missing the fields `options`, `note`, `level`"),
        "{text}"
    );
    assert!(text.contains("local/a.crod:1:1"), "{text}");
    assert!(!text.contains("panicked"), "{text}");
}

#[test]
fn an_unknown_root_key_says_which_fields_there_are() {
    let dir = project("unknown", COMPLETE, "foo: { bar: 2 }\n");
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success(), "{output:?}");
    let text = stderr(&output);
    assert!(
        text.contains(
            "`Config` has no field `foo`; its fields are `name`, `options`, `retries`, `note`, `level`"
        ),
        "{text}"
    );
    assert!(text.contains("local/a.crod:1:"), "{text}");
}

#[test]
fn eval_shows_the_filled_root_and_reads_back() {
    let dir = project("eval", "name: \"x\"\noptions: { a: true }\n", "..\n");
    let output = crol(&dir, &["eval"]);
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    // `level` has a default the schema does not record, so `..` stays.
    assert_eq!(
        text,
        "name: \"x\"\noptions: { a: true }\nretries: 3 // default\nnote: None // default\n..\n"
    );

    std::fs::write(dir.join("base/a.crod"), &text).unwrap();
    std::fs::write(dir.join("local/a.crod"), "").unwrap();
    let again = crol(&dir, &["check"]);
    assert!(again.status.success(), "{again:?}");
}

#[test]
fn all_contexts_reads_back_with_the_same_fingerprint() {
    let dir = project(
        "all-contexts",
        "name: \"x\"\noptions: { a: true }\nnote: \"n\"\n",
        "<prod> { .. }\n..\n-note\n<prod> { retries: 1 }\n",
    );
    let fingerprint = crol(&dir, &["fingerprint"]);
    assert!(fingerprint.status.success(), "{fingerprint:?}");
    let output = crol(&dir, &["eval", "--all-contexts"]);
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    assert!(
        text.lines().any(|l| l.trim_end_matches(',') == ".."),
        "{text}"
    );

    std::fs::write(dir.join("base/a.crod"), &text).unwrap();
    std::fs::write(dir.join("local/a.crod"), "").unwrap();
    let again = crol(&dir, &["fingerprint"]);
    assert!(again.status.success(), "{again:?}");
    assert_eq!(stdout(&fingerprint), stdout(&again), "{text}");
    let check = crol(&dir, &["check"]);
    assert!(check.status.success(), "{check:?}");
}

#[test]
fn defaults_in_only_one_context_read_back() {
    let dir = project(
        "variant-defaults",
        COMPLETE,
        "<prod> { .. }\n<prod> { -name }\n",
    );
    // `name` has no default, also with `..`.
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success(), "{output:?}");
    let dir = project("variant-defaults-2", COMPLETE, "<prod> { .. }\n");
    let fingerprint = crol(&dir, &["fingerprint"]);
    let output = crol(&dir, &["eval", "--all-contexts"]);
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    assert!(text.contains("<prod>"), "{text}");
    std::fs::write(dir.join("base/a.crod"), &text).unwrap();
    std::fs::write(dir.join("local/a.crod"), "").unwrap();
    assert_eq!(stdout(&fingerprint), stdout(&crol(&dir, &["fingerprint"])));
}

#[test]
fn a_removed_field_reads_back_at_a_struct_root() {
    let dir = project("removed", COMPLETE, "-note\n-level\n");
    let check = crol(&dir, &["check"]);
    assert!(check.status.success(), "{check:?}");
    let fingerprint = crol(&dir, &["fingerprint"]);
    for args in [
        &["eval", "--all-contexts"][..],
        &["eval", "--no-defaults"][..],
    ] {
        let dir = project("removed-read-back", COMPLETE, "-note\n-level\n");
        let output = crol(&dir, args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        let text = stdout(&output);
        std::fs::write(dir.join("base/a.crod"), &text).unwrap();
        std::fs::write(dir.join("local/a.crod"), "").unwrap();
        let again = crol(&dir, &["check"]);
        assert!(again.status.success(), "{args:?}: {text}\n{again:?}");
        assert_eq!(
            stdout(&fingerprint),
            stdout(&crol(&dir, &["fingerprint"])),
            "{args:?}: {text}"
        );
    }
}

#[test]
fn a_removed_key_leaves_no_mark_where_the_root_is_no_struct() {
    let dir = project("removed-enum", "a: Options { a: true }\nb: 1\n", "-b\n");
    let project_file = PROJECT.replace(
        "schema: Some(\"schema\"),",
        "schema: Some(\"schema\"), root: Some(\"app::Definition\"),",
    );
    std::fs::write(dir.join("project.crov"), project_file).unwrap();
    let output = crol(&dir, &["eval"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!stdout(&output).contains('b'), "{output:?}");
    let output = crol(&dir, &["eval", "--all-contexts"]);
    assert!(!stdout(&output).contains('b'), "{output:?}");
}

#[test]
fn the_project_root_chooses_the_kind() {
    let dir = project("project-root", "name: \"x\"\n", "");
    let project_file = PROJECT.replace(
        "schema: Some(\"schema\"),",
        "schema: Some(\"schema\"), root: Some(\"app::Definition\"),",
    );
    std::fs::write(dir.join("project.crov"), project_file).unwrap();
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains("expected a `Definition` variant, found \"x\""),
        "{output:?}"
    );
}

#[test]
fn a_standalone_document_is_its_own_whole() {
    let dir = project("standalone", "", "");
    std::fs::write(
        dir.join("one.crod"),
        "#![schema(::schema)]\n\nname: \"x\"\noptions: { a: true }\n",
    )
    .unwrap();
    let output = crol(&dir, &["check", "./one.crod"]);
    assert!(!output.status.success(), "{output:?}");
    let text = stderr(&output);
    assert!(text.contains("`Config` is missing"), "{text}");
    assert!(text.contains("one.crod:1:1"), "{text}");
}

#[test]
fn default_reads_the_field_of_the_root() {
    let dir = project(
        "default",
        "name: \"x\"\noptions: { a: true }\nretries: &default + 1\n",
        "..\n",
    );
    let output = crol(&dir, &["eval", "retries"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stdout(&output).trim(), "4");
}

#[test]
fn no_layers_and_no_keys_are_still_reported_on_nothing() {
    let dir = project("empty", "", "");
    let output = crol(&dir, &["check"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains("`Config` is missing"),
        "{output:?}"
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}
