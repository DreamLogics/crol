//! `crol --init`, run as a process.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crol::RawValue;
use crol::project::{Project, ProjectLayers};

fn crol(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crol"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("crol runs")
}

/// An empty directory for the test, under `base`.
fn fresh_dir(base: &Path, name: &str) -> PathBuf {
    let dir = base.join("init").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An empty Cargo project.
fn cargo_project(name: &str) -> PathBuf {
    let dir = fresh_dir(Path::new(env!("CARGO_TARGET_TMPDIR")), name);
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    dir
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn sets_up_a_cargo_project_that_the_tools_can_load() {
    let dir = cargo_project("success");
    let manifest = std::fs::read(dir.join("Cargo.toml")).unwrap();

    let output = crol(&dir, &["--init"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stderr(&output).contains("crol = {"), "{output:?}");
    assert_eq!(std::fs::read(dir.join("Cargo.toml")).unwrap(), manifest);

    // The application's and the language server's view of the file.
    let path = dir.join("project.crol");
    let project = Project::read(&path).unwrap();
    let ProjectLayers::List(layers) = &project.layers else {
        panic!("{project:?}")
    };
    assert_eq!(layers.len(), 1);
    assert!(
        crol::resolve_module_path(&layers[0].dir, crol::Mode::Data, &dir, None)
            .unwrap()
            .is_dir()
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let value: RawValue = crol::from_str(&text).unwrap();
    let schema = crol::schema::build::<Project>();
    let errors = crol::check_value(&value.0, &schema, None, &Default::default());
    assert!(errors.is_empty(), "{errors:?}");

    // The tools work at once, on formatted files.
    let output = crol(
        &dir,
        &["fmt", "--check", "project.crol", "data/base/main.crod"],
    );
    assert!(output.status.success(), "{output:?}");
    for command in ["check", "eval", "fingerprint"] {
        let output = crol(&dir, &[command]);
        assert!(output.status.success(), "{command}: {output:?}");
    }
}

#[test]
fn the_first_layer_takes_data() {
    let dir = cargo_project("data");
    assert!(crol(&dir, &["--init"]).status.success());
    std::fs::write(dir.join("data/base/main.crod"), "+server: { port: 8080 }\n").unwrap();
    let output = crol(&dir, &["eval"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("port: 8080"));
}

#[test]
fn the_subcommand_does_the_same() {
    let flag = cargo_project("flag");
    let subcommand = cargo_project("subcommand");
    assert!(crol(&flag, &["--init"]).status.success());
    assert!(crol(&subcommand, &["init"]).status.success());
    assert_eq!(
        std::fs::read(flag.join("project.crol")).unwrap(),
        std::fs::read(subcommand.join("project.crol")).unwrap()
    );
}

#[test]
fn finds_the_cargo_project_from_a_directory_below_it() {
    let dir = cargo_project("below");
    let below = dir.join("src/deep");
    std::fs::create_dir_all(&below).unwrap();
    let output = crol(&below, &["--init"]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.join("project.crol").is_file());
    assert!(!below.join("project.crol").exists());
    assert!(crol(&below, &["check"]).status.success());
}

#[test]
fn never_overwrites_a_project_file() {
    let dir = cargo_project("exists");
    std::fs::write(dir.join("project.crol"), "mine").unwrap();
    for args in [&["--init"], &["init"]] {
        let output = crol(&dir, args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stderr(&output).contains("already exists"), "{output:?}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("project.crol")).unwrap(),
        "mine"
    );
    assert!(!dir.join("data").exists());
}

#[test]
fn keeps_the_data_that_is_there() {
    let dir = cargo_project("keeps");
    std::fs::create_dir_all(dir.join("data/base")).unwrap();
    std::fs::write(dir.join("data/base/main.crod"), "+a: 1\n").unwrap();
    let output = crol(&dir, &["--init"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("data/base/main.crod")).unwrap(),
        "+a: 1\n"
    );
}

#[test]
fn refuses_to_make_the_layer_path_ambiguous() {
    let dir = cargo_project("ambiguous");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("data/base.crod"), "+a: 1\n").unwrap();
    let output = crol(&dir, &["--init"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(stderr(&output).contains("in the way"), "{output:?}");
    assert!(!dir.join("project.crol").exists());
    assert!(!dir.join("data/base").exists());
}

#[test]
fn says_when_another_project_file_is_above() {
    let dir = cargo_project("nested");
    std::fs::write(
        dir.join("project.crol"),
        r#"Project { layers: List([]), .. }"#,
    )
    .unwrap();
    let member = dir.join("member");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"member\"\n").unwrap();
    let output = crol(&member, &["--init"]);
    assert!(output.status.success(), "{output:?}");
    assert!(member.join("project.crol").is_file());
    assert!(stderr(&output).contains("exists too"), "{output:?}");
}

#[test]
fn needs_a_cargo_project() {
    // The target directory lies inside this workspace, so outside it; there
    // is nothing to test if the temporary directory is in a Cargo project too.
    let dir = fresh_dir(&std::env::temp_dir(), "crol-no-cargo-project");
    if dir.ancestors().any(|d| d.join("Cargo.toml").is_file()) {
        return;
    }
    let output = crol(&dir, &["--init"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(stderr(&output).contains("no Cargo.toml"), "{output:?}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn errors_follow_the_message_format() {
    let dir = cargo_project("json");
    std::fs::write(dir.join("project.crol"), "mine").unwrap();
    let output = crol(&dir, &["--init", "--message-format", "json"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("{\"severity\":\"error\""), "{stdout}");
}

#[test]
fn the_flag_goes_alone_and_commands_still_take_global_options() {
    let dir = cargo_project("usage");
    let output = crol(&dir, &["--init", "check"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!dir.join("project.crol").exists());

    let output = crol(&dir, &["--color", "never"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("a command is required"),
        "{output:?}"
    );

    let output = crol(&dir, &[]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");

    let output = crol(&dir, &["--color", "never", "init"]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.join("project.crol").is_file());
}

#[test]
fn says_what_the_guide_says() {
    let dir = cargo_project("as-the-guide");
    let output = crol(&dir, &["--init"]);
    assert!(output.status.success(), "{output:?}");
    // `crol` is not on crates.io yet (`docs/getting-started.md`).
    let dependency = "`crol = { git = \"https://github.com/DreamLogics/crol\" }`";
    assert!(stderr(&output).contains(dependency), "{output:?}");

    // The schema where `crol::schema::check_file` puts it (`docs/schemas.md`).
    let project = std::fs::read_to_string(dir.join("project.crol")).unwrap();
    assert!(project.contains("`Some(\"crol::schema\")`"), "{project}");
    assert!(project.contains("`crol/schema.crol`"), "{project}");

    // What the comment suggests works.
    std::fs::create_dir(dir.join("crol")).unwrap();
    let schema = "Schema { format: 1, root: \"t::T\", types: { \"t::T\": Struct { name: \"T\", \
                  source: (\"lib.rs\", 1), fields: {}, .. } } }";
    std::fs::write(dir.join("crol/schema.crol"), schema).unwrap();
    let edited = project.replace("schema: None", "schema: Some(\"crol::schema\")");
    assert_ne!(edited, project);
    std::fs::write(dir.join("project.crol"), edited).unwrap();
    let output = crol(&dir, &["check"]);
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn writes_a_value_document_and_a_data_document() {
    let dir = cargo_project("extensions");
    let output = crol(&dir, &["--init"]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.join("project.crol").is_file());
    assert!(dir.join("data/base/main.crod").is_file());

    // Both load: the project with the composer's layer, the layer as data.
    assert!(crol(&dir, &["check", "project.crol"]).status.success());
    assert!(crol(&dir, &["check", "data::base"]).status.success());
    std::fs::write(dir.join("data/base/main.crod"), "+port: 8080\n").unwrap();
    let output = crol(&dir, &["eval"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "port: 8080\n");
}
