//! Builds and runs the Rust blocks of the documentation: the guide in `docs/`
//! and the READMEs. A page is read from top to bottom, as a reader would:
//!
//! - A `crol` block whose first line is `// path` is a file, and each program
//!   after it runs in a directory holding the page's files so far. A
//!   `$ mv a b` in a `console` block moves one.
//! - A `rust` block with a `main` is built and run, and it must succeed. If a
//!   `console` block with `$ cargo run` comes next, it must print what that
//!   shows, and `$ NAME=value cargo run` sets the variable.
//! - The first `// src/lib.rs` block is the crate that `name` in the page's
//!   `Cargo.toml` block says, for the blocks after it. Every block without a
//!   `main` is built, and `#[test]` functions in `// tests/...` are run.
//! - `rust,no_run` is built only, and `rust,ignore` is skipped, as in rustdoc.
//!
//! All the programs are binaries of one Cargo project in the target directory,
//! so that they share their dependencies, and a later run builds what changed.

#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A fenced block of a page.
struct Block {
    /// The line of the opening fence.
    line: usize,
    /// What follows the opening fence.
    info: String,
    text: String,
}

/// A program made from a `rust` block.
struct Program {
    /// Where it is, for messages.
    place: String,
    /// The name of its binary.
    bin: String,
    source: String,
    /// The page's files at this point.
    files: BTreeMap<String, String>,
    run: bool,
    /// Variables to set.
    env: Vec<(String, String)>,
    /// What it must print.
    expected: Option<String>,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The pages, relative to the root of the repository.
fn pages() -> Vec<PathBuf> {
    let root = root();
    let in_directory = |directory: &str| -> Vec<PathBuf> {
        fs::read_dir(root.join(directory))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    };
    let guide = in_directory("docs");
    let crates = in_directory("crates");
    let readmes = crates.into_iter().map(|c| c.join("README.md"));
    let mut pages: Vec<PathBuf> = guide
        .into_iter()
        .chain(readmes)
        .chain([root.join("README.md")])
        .filter(|path| path.extension().is_some_and(|e| e == "md") && path.exists())
        .map(|path| path.strip_prefix(&root).unwrap().to_owned())
        .collect();
    pages.sort();
    pages
}

fn blocks(page: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<Block> = None;
    for (number, line) in page.lines().enumerate() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(info)) => {
                open = Some(Block {
                    line: number + 1,
                    info: info.to_owned(),
                    text: String::new(),
                });
            }
            (Some(_), Some("")) => blocks.extend(open.take()),
            (Some(block), _) => {
                block.text.push_str(line);
                block.text.push('\n');
            }
            (None, None) => {}
        }
    }
    blocks
}

/// What the first line of a block names: `// path`.
fn named(text: &str) -> Option<&str> {
    let name = text.lines().next()?.strip_prefix("// ")?;
    (!name.contains(' ')).then_some(name)
}

fn has_main(source: &str) -> bool {
    source.lines().any(|line| line.starts_with("fn main("))
}

/// The `#[test]` functions of a source, as a `main` that calls them, without
/// the attribute, which would take them out of a binary.
fn tests_as_main(source: &str) -> String {
    let mut calls = String::new();
    let mut lines = source.lines().peekable();
    let mut out = String::new();
    while let Some(line) = lines.next() {
        if line == "#[test]" {
            let function = lines.peek().and_then(|l| l.strip_prefix("fn "));
            let name = function.and_then(|f| f.split('(').next()).unwrap();
            calls.push_str(&format!("{name}();"));
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&format!("fn main() {{ {calls} }}\n"));
    out
}

/// The variables and the output of a `console` block that starts with
/// `cargo run`.
fn cargo_run(console: &str) -> Option<(Vec<(String, String)>, String)> {
    let (command, rest) = console.split_once('\n')?;
    let command = command.strip_prefix("$ ")?;
    let (variables, _) = command
        .rsplit_once("cargo run")
        .filter(|(_, c)| c.is_empty())?;
    let env = variables
        .split_whitespace()
        .filter_map(|variable| variable.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.trim_matches('\'').to_owned()))
        .collect();
    let output = rest.lines().take_while(|line| !line.starts_with("$ "));
    Some((env, output.map(|line| format!("{line}\n")).collect()))
}

/// Applies each `$ mv a b` of a `console` block.
fn moves(console: &str, files: &mut BTreeMap<String, String>) {
    for line in console.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let ["$", "mv", from, to] = words[..]
            && let Some(text) = files.remove(from)
        {
            files.insert(to.to_owned(), text);
        }
    }
}

fn programs(path: &Path, page: &str) -> Vec<Program> {
    let mut programs: Vec<Program> = Vec::new();
    let mut files = BTreeMap::new();
    let mut package = None;
    let mut library: Option<String> = None;
    let mut after_program = false;
    for Block { line, info, text } in blocks(page) {
        let (language, mode) = info.split_once(',').unwrap_or((&info, ""));
        let follows = std::mem::take(&mut after_program);
        match language {
            "crol" => {
                if let Some(name) = named(&text) {
                    files.insert(name.to_owned(), text);
                }
            }
            "toml" => {
                let name = text.lines().find_map(|l| l.strip_prefix("name = "));
                package = package.or(name.map(|name| name.trim_matches('"').to_owned()));
            }
            "console" => {
                moves(&text, &mut files);
                if let (true, Some((env, output))) = (follows, cargo_run(&text)) {
                    let program = programs.last_mut().unwrap();
                    program.env = env;
                    program.expected = Some(output);
                }
            }
            "rust" if mode == "ignore" => {}
            "rust" => {
                let name = named(&text);
                let is_test = name.is_some_and(|name| name.starts_with("tests/"));
                let mut source = if is_test {
                    tests_as_main(&text)
                } else {
                    text.clone()
                };
                let run = mode != "no_run" && has_main(&source);
                if !has_main(&source) {
                    source.push_str("fn main() {}\n");
                }
                if name == Some("src/lib.rs") {
                    library.get_or_insert(text);
                } else if let (Some(package), Some(library)) = (&package, &library)
                    && source.contains(&format!("{package}::"))
                {
                    source.push_str(&format!("mod {package} {{\n{library}}}\n"));
                }
                let stem = path.with_extension("").display().to_string();
                let stem = stem.replace(|c: char| !c.is_alphanumeric(), "_");
                programs.push(Program {
                    place: format!("{}:{line}", path.display()),
                    bin: format!("{stem}_{line}").to_lowercase(),
                    source,
                    files: files.clone(),
                    run,
                    env: Vec::new(),
                    expected: None,
                });
                after_program = true;
            }
            _ => {}
        }
    }
    programs
}

/// Writes a file unless it holds that already, so that Cargo sees what changed.
fn write(path: &Path, text: &str) {
    if fs::read_to_string(path).is_ok_and(|old| old == text) {
        return;
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The Cargo project of the programs. The lock file of the repository keeps
/// the dependencies at the versions everything else uses.
fn project(programs: &[Program]) -> PathBuf {
    let root = root().canonicalize().unwrap();
    let crate_path = |name: &str| root.join("crates").join(name).display().to_string();
    let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join("guide");
    let manifest = format!(
        r#"[package]
name = "guide"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
crol = {{ path = {crol:?}, features = ["derive"] }}
crol-eval = {{ path = {eval:?} }}
crol-lsp = {{ path = {lsp:?} }}
crol-syntax = {{ path = {syntax:?} }}
lsp-server = "*"
serde = {{ version = "1", features = ["derive"] }}

[profile.dev]
debug = false

[workspace]
"#,
        crol = crate_path("crol"),
        eval = crate_path("crol-eval"),
        lsp = crate_path("crol-lsp"),
        syntax = crate_path("crol-syntax"),
    );
    write(&directory.join("Cargo.toml"), &manifest);
    if !directory.join("Cargo.lock").exists() {
        fs::copy(root.join("Cargo.lock"), directory.join("Cargo.lock")).unwrap();
    }
    let bin = directory.join("src/bin");
    let stale = fs::read_dir(&bin)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().path());
    for path in stale.filter(|path| path.extension().is_some_and(|e| e == "rs")) {
        let stem = path.file_stem().unwrap();
        if !programs.iter().any(|program| stem == program.bin.as_str()) {
            fs::remove_file(path).unwrap();
        }
    }
    for program in programs {
        write(
            &bin.join(&program.bin).with_extension("rs"),
            &program.source,
        );
        // What `include_str!` finds beside `src/main.rs`.
        for (name, text) in &program.files {
            if let Some(name) = name.strip_prefix("src/") {
                write(&bin.join(name), text);
            }
        }
    }
    directory
}

fn run(program: &Program, directory: &Path) -> Result<(), String> {
    let sandbox = directory.join("run").join(&program.bin);
    if sandbox.exists() {
        fs::remove_dir_all(&sandbox).unwrap();
    }
    fs::create_dir_all(&sandbox).unwrap();
    for (name, text) in &program.files {
        write(&sandbox.join(name), text);
    }
    let output = Command::new(directory.join("target/debug").join(&program.bin))
        .current_dir(&sandbox)
        // `check_file` goes by this, and this test's own would be the repository.
        .env("CARGO_MANIFEST_DIR", &sandbox)
        .envs(program.env.iter().map(|(name, value)| (name, value)))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("it failed: {stderr}{stdout}"));
    }
    match &program.expected {
        Some(expected) if *expected != stdout => Err(format!(
            "the page shows\n{expected}and it printed\n{stdout}"
        )),
        _ => Ok(()),
    }
}

/// Cargo's output without its warnings and its progress.
fn errors(output: &str) -> String {
    let lines = output.lines().filter(|line| line.contains("error"));
    lines.collect::<Vec<_>>().join("\n")
}

#[test]
fn rust_blocks_build_and_run() {
    let mut all = Vec::new();
    for path in pages() {
        let page = fs::read_to_string(root().join(&path)).unwrap();
        all.extend(programs(&path, &page));
    }
    assert!(all.len() > 40, "only {} blocks were found", all.len());

    let directory = project(&all);
    let build = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--bins", "--keep-going", "--message-format=short"])
        .current_dir(&directory)
        .env("CARGO_TARGET_DIR", directory.join("target"))
        // CI denies warnings, and an example need not read every field.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "a block does not build (`docs_rust_684` is `docs/rust.md`, line 684, and lines \
         count from the fence):\n{}",
        errors(&String::from_utf8_lossy(&build.stderr))
    );

    let failures: Vec<String> = all
        .iter()
        .filter(|program| program.run)
        .filter_map(|program| {
            let failed = run(program, &directory).err()?;
            Some(format!("{}: {failed}", program.place))
        })
        .collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
