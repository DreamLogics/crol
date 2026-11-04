//! A file's extension states its mode (`SPEC.md` 4): each rule, with a
//! passing and a failing case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crol::project::Project;
use crol::schema::{self, SchemaFile};
use crol::{
    CrolComposer, CrolConfig, CrolLayer, FileLoader, FileLoaders, Loaded, LoaderError, Mode,
    ModulePath, SourceId,
};

/// An empty directory for the test, with `files` in it.
fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("modes")
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

fn composer(root: &Path) -> CrolComposer {
    CrolComposer::with_cfg(CrolConfig::<String> {
        root_path: root.to_path_buf(),
        ..Default::default()
    })
}

const PROJECT: &str = "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }";

fn schema_text() -> String {
    schema::build::<Project>().to_text().unwrap()
}

#[test]
fn a_layer_directory_holds_crod_files_and_ignores_what_is_no_document() {
    let dir = tree(
        "layer-good",
        &[
            ("data/a.crod", "+a: 1,"),
            ("data/sub/b.crod", "+b: 2,"),
            ("data/README.md", "not a document"),
            ("data/.hidden.crol", "1"),
            ("data/.git/x.crol", "1"),
        ],
    );
    let data = composer(&dir)
        .load("::data")
        .unwrap()
        .compose_data()
        .unwrap();
    assert_eq!(data.keys(), ["a", "b"]);
}

#[test]
fn a_crol_file_in_a_layer_directory_is_an_error() {
    let dir = tree(
        "layer-crol",
        &[("data/a.crod", "+a: 1,"), ("data/sub/b.crol", "2")],
    );
    for error in [
        composer(&dir).load("::data").map(|_| ()).unwrap_err(),
        composer(&dir)
            .load(dir.join("data"))
            .map(|_| ())
            .unwrap_err(),
        CrolLayer::from_path("base", &dir.join("data"))
            .map(|_| ())
            .unwrap_err(),
    ] {
        let message = error.to_string();
        assert!(message.contains("b.crol"), "{message}");
        assert!(message.contains("cannot be a layer"), "{message}");
        assert!(message.contains("rename it to `b.crod`"), "{message}");
        assert!(error.source_id().is_some());
    }
}

#[test]
fn a_file_given_as_a_layer_must_not_state_another_mode() {
    let dir = tree(
        "direct",
        &[
            ("a.crod", "+a: 1,"),
            ("b.crol", "2"),
            ("d.conf", "+d: 4,"),
            ("e", "+e: 5,"),
        ],
    );
    let load = |name: &str| composer(&dir).load(dir.join(name)).map(|_| ());
    load("a.crod").unwrap();
    let message = load("b.crol").unwrap_err().to_string();
    assert!(message.contains("cannot be a layer"), "{message}");

    // Without an extension that states one, the API does: data mode.
    for name in ["d.conf", "e"] {
        let data = composer(&dir)
            .load(dir.join(name))
            .unwrap()
            .compose_data()
            .unwrap();
        assert_eq!(data.keys().len(), 1);
    }
}

#[test]
fn text_has_no_extension_so_the_call_states_the_mode() {
    // `from_str` is value mode.
    assert_eq!(crol::from_str::<i32>("40 + 2").unwrap(), 42);

    // The composer is data mode, whatever a document in memory is called.
    for name in ["a.crod", "a.crol", "a.txt", "a"] {
        let layer = CrolLayer::from_sources("base", [(name, "+a: 1,")]);
        let data = CrolComposer::new()
            .load_layer(layer)
            .compose_data()
            .unwrap();
        assert_eq!(data.get::<u8>("a").unwrap(), 1);

        let layer = CrolLayer::from_sources("base", [(name, "#![type(Config)]\n\na: 1,")]);
        let error = CrolComposer::new()
            .load_layer(layer)
            .compose_data()
            .unwrap_err();
        assert!(error.to_string().contains("type"), "{error}");
    }
}

#[test]
fn a_module_path_names_a_crod_file_or_a_directory() {
    let dir = tree(
        "module-paths",
        &[
            ("data/file.crod", "+a: 1,"),
            ("data/dir/x.crod", "+b: 2,"),
            ("data/value.crol", "1"),
        ],
    );
    composer(&dir).load("data::file").unwrap();
    composer(&dir).load("data::dir").unwrap();
    let error = |path: &str| composer(&dir).load(path).map(|_| ()).unwrap_err();
    assert!(
        error("data::value")
            .to_string()
            .contains("cannot be a layer")
    );
    assert!(error("data::none").to_string().contains("names nothing"));
    assert!(error("super::x").to_string().contains("above the root"));
}

#[test]
fn extends_is_data_mode() {
    let dir = tree(
        "extends",
        &[
            ("base.crod", "+port: 80,"),
            ("good.crod", "#![extends(base)]\n\nport: & + 1,"),
            ("value.crol", "{ port: 80 }"),
            ("wrong.crod", "#![extends(value)]\n\nport: & + 1,"),
        ],
    );
    let load = |path: &str| {
        CrolComposer::with_cfg(CrolConfig::<String> {
            root_path: dir.clone(),
            load_extends: true,
            ..Default::default()
        })
        .load(path)
    };
    let data = load("good").unwrap().compose_data().unwrap();
    assert_eq!(data.get::<u16>("port").unwrap(), 81);
    let message = load("wrong").map(|_| ()).unwrap_err().to_string();
    assert!(message.contains("cannot be a layer"), "{message}");
}

#[test]
fn a_project_file_is_a_crol_file() {
    let dir = tree(
        "project-file",
        &[
            ("project.crol", PROJECT),
            ("data.crod", PROJECT),
            ("plain", PROJECT),
        ],
    );
    Project::read(&dir.join("project.crol")).unwrap();
    Project::read(&dir.join("plain")).unwrap();
    let message = Project::read(&dir.join("data.crod"))
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("cannot be read as a value document"),
        "{message}"
    );
    assert!(message.contains("rename it to `data.crol`"), "{message}");
}

#[test]
fn a_project_file_is_found_by_its_name_alone() {
    let dir = tree(
        "project-find",
        &[("a/project.crol", PROJECT), ("a/b/c", "")],
    );
    let found = Project::find(&dir.join("a/b")).unwrap();
    assert_eq!(found, dir.join("a/project.crol"));

    // Only `project.crol` is looked for: another name is not a project.
    let dir = tree(
        "project-other-names",
        &[("project.crod", PROJECT), ("project.conf", PROJECT)],
    );
    assert_eq!(Project::find(&dir), None);
}

#[test]
fn a_schema_file_is_a_crol_file() {
    let text = schema_text();
    let dir = tree(
        "schema-file",
        &[("schema.crol", &text), ("data.crod", &text)],
    );
    SchemaFile::read(dir.join("schema.crol")).unwrap();
    let message = SchemaFile::read(dir.join("data.crod"))
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("cannot be read as a value document"),
        "{message}"
    );

    // Writing one is the same.
    schema::write::<Project>(dir.join("written.crol")).unwrap();
    assert!(schema::write::<Project>(dir.join("written.crod")).is_err());
    assert!(schema::check_file::<Project>(dir.join("schema.crol")).is_ok());
    assert!(schema::check_file::<Project>(dir.join("data.crod")).is_err());
}

#[test]
fn schema_paths_name_crol_files() {
    let text = schema_text();
    let dir = tree(
        "schema-paths",
        &[("good.crol", &text), ("data.crod", &text)],
    );
    let attribute = |name: &str| {
        let document = format!("#![schema(::{name})]\n\n1");
        schema::attribute_path(&document, &dir.join("a.crol"), &dir, false)
            .unwrap()
            .1
    };
    assert_eq!(attribute("good").unwrap(), dir.join("good.crol"));
    let message = attribute("data").unwrap_err().to_string();
    assert!(
        message.contains("cannot be read as a value document"),
        "{message}"
    );

    let project = |name: &str| {
        let text = format!("Project {{ layers: List([]), schema: Some(\"{name}\"), .. }}");
        crol::from_str::<Project>(&text)
            .unwrap()
            .schema_path(&dir, false)
    };
    assert_eq!(project("good").unwrap().unwrap(), dir.join("good.crol"));
    assert!(project("data").is_err());
}

#[test]
fn the_project_file_is_the_one_crol_file_a_layer_may_hold() {
    let dir = tree(
        "project-in-layer",
        &[("project.crol", PROJECT), ("a.crod", "+a: 1,")],
    );
    // Not named as the project file, it is any other value document.
    assert!(CrolLayer::from_path("base", &dir).is_err());

    let loaders = FileLoaders::new().with_project_file(dir.join("project.crol"));
    let data = CrolComposer::with_cfg(CrolConfig::<String> {
        file_loaders: loaders.clone(),
        ..Default::default()
    })
    .load(dir.clone())
    .unwrap()
    .compose_data()
    .unwrap();
    assert_eq!(data.keys(), ["a"]);

    // Named, it does not excuse another.
    std::fs::write(dir.join("other.crol"), "1").unwrap();
    let error = CrolComposer::with_cfg(CrolConfig::<String> {
        file_loaders: loaders,
        ..Default::default()
    })
    .load(dir.clone())
    .map(|_| ())
    .unwrap_err();
    assert!(error.to_string().contains("other.crol"), "{error}");
}

/// Documents by name, as a custom loader would have them: the source's name
/// is `prefix` and the path given here.
struct Custom {
    documents: BTreeMap<&'static str, &'static str>,
    /// The directory `::listed` is, in the order it is listed.
    listed: Vec<&'static str>,
}

impl Custom {
    fn new(documents: &[(&'static str, &'static str)]) -> Self {
        Self {
            documents: documents.iter().copied().collect(),
            listed: Vec::new(),
        }
    }
}

impl FileLoader for Custom {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        let name = path.segments().join("/");
        if name == "listed" {
            return Ok(Some(SourceId::new("custom:listed")));
        }
        // What the module path is looked for as chooses the file, and then
        // anything without an extension.
        let with_extension = format!("{name}.{}", path.mode().extension());
        Ok([with_extension.as_str(), name.as_str()]
            .into_iter()
            .find(|key| self.documents.contains_key(key))
            .map(|key| SourceId::new(format!("custom:{key}"))))
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        let name = source.as_name().unwrap().trim_start_matches("custom:");
        if name == "listed" {
            let entries = self
                .listed
                .iter()
                .map(|path| ((*path).to_owned(), SourceId::new(format!("custom:{path}"))));
            return Ok(Loaded::Directory(entries.collect()));
        }
        Ok(Loaded::Document(self.documents[name].as_bytes().to_vec()))
    }
}

/// Sources may be named anything: this one's are, whatever it was asked.
struct Misnamed(&'static str);

impl FileLoader for Misnamed {
    fn resolve(&self, _: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        Ok(Some(SourceId::new(self.0)))
    }

    fn load(&self, _: &SourceId) -> Result<Loaded, LoaderError> {
        Ok(Loaded::Document(b"+a: 1,".to_vec()))
    }
}

fn custom_composer(loader: impl FileLoader + 'static) -> CrolComposer {
    CrolComposer::new()
        .with_file_loader(loader)
        .without_file_system()
}

#[test]
fn a_loader_is_told_what_it_is_looked_for_as() {
    let custom = Custom::new(&[("a.crod", "+a: 1,"), ("a.crol", "1"), ("plain", "+p: 1,")]);
    let loaders = FileLoaders::new().with(custom).without_file_system();
    let root = Path::new("");
    let data = loaders.resolve("::a", root).unwrap();
    assert_eq!(data.id().to_string(), "custom:a.crod");
    assert_eq!(data.text().unwrap(), "+a: 1,");
    let value = loaders.resolve_value("::a", root).unwrap();
    assert_eq!(value.id().to_string(), "custom:a.crol");
    assert_eq!(value.text().unwrap(), "1");

    // A name without a recognised extension follows the API: either.
    let plain = loaders.resolve("::plain", root).unwrap();
    assert_eq!(plain.load("base").unwrap().documents().count(), 1);
    assert_eq!(
        loaders
            .resolve_value("::plain", root)
            .unwrap()
            .text()
            .unwrap(),
        "+p: 1,"
    );
}

#[test]
fn a_custom_source_with_a_recognised_extension_is_held_to_it() {
    custom_composer(Misnamed("embedded:data/base.crod"))
        .load("::data::base")
        .unwrap();
    custom_composer(Misnamed("embedded:42"))
        .load("::data::base")
        .unwrap();
    let error = |name| {
        custom_composer(Misnamed(name))
            .load("::data::base")
            .map(|_| ())
            .unwrap_err()
    };
    let wrong = error("embedded:data/base.crol");
    assert!(wrong.to_string().contains("cannot be a layer"), "{wrong}");
    assert_eq!(
        wrong.source_id(),
        Some(&SourceId::new("embedded:data/base.crol"))
    );

    let loaders = |name| {
        FileLoaders::new()
            .with(Misnamed(name))
            .without_file_system()
            .resolve_value("::schema", Path::new(""))
            .unwrap()
    };
    assert!(loaders("embedded:schema.crol").text().is_ok());
    assert!(loaders("embedded:schema").text().is_ok());
    let error = loaders("embedded:schema.crod").text().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot be read as a value document"),
        "{error}"
    );
}

#[test]
fn what_a_custom_directory_lists_is_held_to_it_in_path_order() {
    let documents = [
        ("a.crod", "+a: 1,"),
        ("b", "+b: 2,"),
        ("c.crol", "1"),
        ("d.crol", "+d: 4,"),
    ];
    let mut good = Custom::new(&documents);
    good.listed = vec!["b", "a.crod"];
    let data = custom_composer(good)
        .load("::listed")
        .unwrap()
        .compose_data()
        .unwrap();
    assert_eq!(data.keys(), ["a", "b"]);

    // The same error whatever order the loader lists them in.
    for listed in [
        vec!["d.crol", "c.crol", "a.crod"],
        vec!["a.crod", "c.crol", "d.crol"],
    ] {
        let mut bad = Custom::new(&documents);
        bad.listed = listed;
        let error = custom_composer(bad)
            .load("::listed")
            .map(|_| ())
            .unwrap_err();
        assert!(error.to_string().contains("custom:c.crol"), "{error}");
    }
}

#[test]
fn modes_are_named_by_their_extensions() {
    assert_eq!(Mode::Data.extension(), "crod");
    assert_eq!(Mode::Value.extension(), "crol");
    assert_eq!(Mode::of("a.crol"), Some(Mode::Value));
    assert_eq!(Mode::of_path(Path::new("a.crod")), Some(Mode::Data));
    assert_eq!(Mode::of("a.txt"), None);
}
