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
    let path = file("in_place.crod", "+web:{port:80}");
    let output = crol(&["fmt", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "+web: { port: 80 }\n"
    );
}

#[test]
fn check_reports_unformatted_files_without_changing_them() {
    let formatted = file("check_ok.crod", "a: 1\n");
    let unformatted = file("check_bad.crod", "a:1");
    let output = crol(&["fmt", "--check", formatted.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");

    let output = crol(&["fmt", "--check", unformatted.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not formatted"));
    assert_eq!(std::fs::read_to_string(&unformatted).unwrap(), "a:1");
}

#[test]
fn syntax_errors_fail_and_leave_the_file_alone() {
    let path = file("syntax_error.crod", "a: 1 b: 2");
    let output = crol(&["fmt", "--color", "never", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("expected `,` or a line break between entries"),
        "{stderr}"
    );
    assert!(stderr.contains("syntax_error.crod:1:5"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a: 1 b: 2");
}

#[test]
fn invalid_utf8_is_an_error() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fmt");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("not_utf8.crod");
    std::fs::write(&path, b"a: \"\xFF\"").unwrap();
    let output = crol(&["fmt", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not valid UTF-8"));
}

#[test]
fn from_ron_converts_to_crol() {
    let path = file(
        "config.ron",
        "Config(name: \"app\", ports: [80, 443], mode: Fast)",
    );
    let output = crol(&["from-ron", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Config { name: \"app\", ports: [80, 443], mode: Fast, .. }\n"
    );
}

#[test]
fn check_and_eval_read_layers() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("layers");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("base")).unwrap();
    std::fs::create_dir_all(dir.join("local")).unwrap();
    std::fs::write(
        dir.join("base/a.crod"),
        "+web: { port: 80, url: \"x:\" + &self.port },",
    )
    .unwrap();
    std::fs::write(dir.join("local/a.crod"), "web.port: 81,").unwrap();
    let base = dir.join("base");
    let local = dir.join("local");
    let (base, local) = (base.to_str().unwrap(), local.to_str().unwrap());

    let output = crol(&["check", base, local]);
    assert!(output.status.success(), "{output:?}");

    let output = crol(&["eval", base, local]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "web: { port: 81, url: \"x:81\" }\n"
    );
    let output = crol(&["eval", base, local, "web.url"]);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "\"x:81\"\n");

    std::fs::write(dir.join("local/b.crod"), "+web: 1,").unwrap();
    let output = crol(&["check", "--color", "never", base, local]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("+: `web` already exists"));
}

#[test]
fn variants_contexts_and_fingerprints() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("variants");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.crod"),
        "+web: { replicas: 1 },\n<prod> { web.replicas: 4 },",
    )
    .unwrap();
    let layer = dir.join("a.crod");
    let layer = layer.to_str().unwrap();
    let declared = ["--group", "dev,prod"];

    let mut args = vec!["eval", layer, "--context", "prod", "web.replicas"];
    args.extend(declared);
    assert_eq!(String::from_utf8_lossy(&crol(&args).stdout), "4\n");

    let mut args = vec!["eval", layer, "--all-contexts"];
    args.extend(declared);
    assert_eq!(
        String::from_utf8_lossy(&crol(&args).stdout),
        "web: { replicas: 1 }\n\n<!dev & prod> {\n    web: { replicas: 4 },\n}\n"
    );

    let mut args = vec!["fingerprint", layer];
    args.extend(declared);
    let first = crol(&args);
    assert!(first.status.success(), "{first:?}");
    assert_eq!(first.stdout.len(), 65);
    assert_eq!(crol(&args).stdout, first.stdout);
}

#[test]
fn explain_prints_the_history_of_a_value() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("explain");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.crod"),
        "+web: { port: 80 },\nweb.port: & + 1,\n<prod> { web.port: 443 },",
    )
    .unwrap();
    let layer = dir.join("a.crod");
    let layer = layer.to_str().unwrap();
    let output = crol(&[
        "explain",
        layer,
        "--discrete",
        "prod",
        "--context",
        "prod",
        "web.port",
    ]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<Vec<&str>> = stdout
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(lines[0], ["web.port", "in", "{prod}:"]);
    assert_eq!(lines[1][1..], ["+web", "definition", "80"], "{stdout}");
    assert_eq!(lines[2][1..], ["web.port", "patch", "81"], "{stdout}");
    assert_eq!(
        lines[3][1..],
        ["web<prod>.port", "patch", "443"],
        "{stdout}"
    );
}

#[test]
fn project_files_give_the_layers_and_the_schema() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("project");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data/base")).unwrap();
    std::fs::create_dir_all(dir.join("crol")).unwrap();
    std::fs::write(
        dir.join("project.crov"),
        r#"Project {
            layers: List([{ name: "base", dir: "data::base", .. }]),
            variants: [Grouped(["dev", "prod"])],
            schema: Some("crol::schema"),
            ..
        }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("data/base/a.crod"),
        "+web: Service { replicas: 1 },\n<prod> { web.replicas: -4 },",
    )
    .unwrap();
    std::fs::write(
        dir.join("crol/schema.crov"),
        r#"Schema {
            format: 1,
            root: "app::Service",
            types: {
                "app::Service": Struct {
                    name: "Service",
                    source: ("src/lib.rs", 1),
                    fields: { replicas: { ty: U32, .. } },
                    ..
                },
            },
        }"#,
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_crol"))
            .args(args)
            .current_dir(dir.join("data"))
            .output()
            .unwrap()
    };
    let output = run(&["check", "--color", "never"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("in {prod}: expected u32, found -4"),
        "{stderr}"
    );

    let output = run(&["eval", "--context", "dev"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "web: Service { replicas: 1 }\n"
    );
}

#[test]
fn documents_in_a_project_should_name_its_schema() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("project-schemas");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("base")).unwrap();
    std::fs::write(
        dir.join("project.crov"),
        r#"Project { layers: List([{ name: "base", dir: "base", .. }]), schema: Some("schema"), .. }"#,
    )
    .unwrap();
    let schema = r#"Schema { format: 1, root: "app::Service", types: {} }"#;
    std::fs::write(dir.join("schema.crov"), schema).unwrap();
    std::fs::write(dir.join("other.crov"), schema).unwrap();
    std::fs::write(dir.join("base/a.crod"), "#![schema(::schema)]\n+a: 1,\n").unwrap();
    std::fs::write(
        dir.join("base/b.crod"),
        "#![schema(super::other)]\n+b: 1,\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_crol"))
        .args(["check", "--color", "never"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("warning").count(), 1, "{stderr}");
    assert!(
        stderr.contains(
            "this document names another schema than the project's, which is used instead"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("b.crod:1:4"), "{stderr}");
}

#[test]
fn check_uses_the_schema_a_document_names() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("schema-attribute");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("schema.crov"),
        r#"Schema {
            format: 1,
            root: "app::Service",
            types: {
                "app::Service": Struct {
                    name: "Service",
                    source: ("src/lib.rs", 1),
                    fields: { replicas: { ty: U32, .. } },
                    ..
                },
            },
        }"#,
    )
    .unwrap();
    let good = dir.join("good.crod");
    let bad = dir.join("bad.crod");
    let missing = dir.join("missing.crod");
    std::fs::write(
        &good,
        "#![schema(schema)]\n+web: Service { replicas: 2 },\n",
    )
    .unwrap();
    std::fs::write(
        &bad,
        "#![schema(schema)]\n+web: Service { replicas: -2 },\n",
    )
    .unwrap();
    std::fs::write(&missing, "#![schema(::nothing)]\n+web: 1,\n").unwrap();

    let output = crol(&["check", good.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    let output = crol(&["check", "--color", "never", bad.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("expected u32, found -2"), "{stderr}");
    let output = crol(&["check", "--color", "never", missing.to_str().unwrap()]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("schema: the module path `::nothing`"),
        "{stderr}"
    );
}

#[test]
fn eval_shows_the_defaults_fields_take() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("eval-defaults");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("schema.crov"),
        r#"Schema {
            format: 1,
            root: "app::Service",
            types: {
                "app::Service": Struct {
                    name: "Service",
                    source: ("src/lib.rs", 1),
                    fields: {
                        replicas: { ty: U32, default: Value(1), .. },
                        image: { ty: String, .. },
                        tags: { ty: List(String), default: Unrecorded, .. },
                    },
                    ..
                },
            },
        }"#,
    )
    .unwrap();
    let data = dir.join("data.crod");
    std::fs::write(
        &data,
        "+web: Service { image: \"w\", .. },\n\
         +db: Service { image: \"d\", replicas: 2, tags: [] },\n\
         db: & ~ { -replicas },\n",
    )
    .unwrap();
    let schema = dir.join("schema.crov");
    let eval = |extra: &[&str]| {
        let mut args = vec![
            "eval",
            data.to_str().unwrap(),
            "--schema",
            schema.to_str().unwrap(),
        ];
        args.extend(extra);
        let output = crol(&args);
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    // A default the schema does not record stays left out, with `..`.
    assert_eq!(
        eval(&[]),
        "web: Service {\n    image: \"w\",\n    replicas: 1, // default\n    ..,\n}\n\
         db: Service {\n    image: \"d\",\n    tags: [],\n    replicas: 1, // default\n}\n"
    );
    assert_eq!(
        eval(&["--no-defaults"]),
        "web: Service { image: \"w\", .. }\n\
         db: Service { image: \"d\", tags: [], replicas: (), -replicas }\n"
    );
}
