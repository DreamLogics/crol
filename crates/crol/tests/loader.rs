//! File loaders (`SPEC.md` 10.7).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crol::schema::SchemaFile;
use crol::{
    CrolComposer, CrolConfig, CrolLayer, FileLoader, FileLoaders, FsLoader, Loaded, LoaderError,
    ModulePath, SourceId,
};

/// A module path a loader was given: its segments, and as written.
type Asked = (Vec<String>, String);

/// Documents in memory, by path, as a file system would have them: a module
/// path names `name.crod`, `name.crol` for a value document, or the directory
/// `name`.
#[derive(Default, Clone)]
struct Memory {
    files: BTreeMap<String, String>,
    /// Paths that fail to load.
    failing: Vec<String>,
    /// Every module path asked for, as segments and as written.
    asked: Arc<Mutex<Vec<Asked>>>,
}

impl Memory {
    fn new(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
                .collect(),
            ..Self::default()
        }
    }

    fn id(path: &str) -> SourceId {
        SourceId::new(format!("mem:{path}"))
    }
}

impl FileLoader for Memory {
    fn resolve(&self, path: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
        self.asked
            .lock()
            .unwrap()
            .push((path.segments().to_vec(), path.written().to_owned()));
        let name = path.segments().join("/");
        let file = format!("{name}.{}", path.mode().extension());
        if self.files.contains_key(&file) {
            return Ok(Some(Self::id(&file)));
        }
        let below = format!("{name}/");
        Ok(self
            .files
            .keys()
            .any(|f| f.starts_with(&below))
            .then(|| Self::id(&name)))
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, LoaderError> {
        let name = source.as_name().unwrap().strip_prefix("mem:").unwrap();
        if self.failing.iter().any(|f| f == name) {
            return Err("the disk is on fire".into());
        }
        if let Some(text) = self.files.get(name) {
            return Ok(Loaded::Document(text.clone().into_bytes()));
        }
        let below = format!("{name}/");
        // Newest first, so that the order is not the one `crol` needs.
        Ok(Loaded::Directory(
            self.files
                .keys()
                .rev()
                .filter_map(|f| {
                    let relative = f.strip_prefix(&below)?;
                    Some((relative.to_owned(), Self::id(f)))
                })
                .collect(),
        ))
    }
}

/// A composer with nothing but `loader`, following `extends`.
fn composer(loader: impl FileLoader + 'static) -> CrolComposer {
    CrolComposer::with_cfg(CrolConfig::<String> {
        load_extends: true,
        ..Default::default()
    })
    .with_file_loader(loader)
    .without_file_system()
}

fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}

#[test]
fn composes_layers_with_no_file_system() {
    let memory = Memory::new(&[
        ("data/base/web.crod", "+port: 80,"),
        ("data/base/nested/db.crod", "+db: 5432,"),
        ("data/local.crod", "port: & + 1,"),
    ]);
    let composed = composer(memory)
        .load("::data::base")
        .unwrap()
        .load("::data::local")
        .unwrap();
    // Documents are in path order, whatever order the loader gave.
    let layers = composed.layers();
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].name(), "::data::base");
    let names: Vec<_> = layers[0].documents().map(|(r, n, _)| (r, n)).collect();
    assert_eq!(
        names,
        [
            ("nested/db.crod", "mem:data/base/nested/db.crod"),
            ("web.crod", "mem:data/base/web.crod"),
        ]
    );
    assert_eq!(layers[1].documents().next().unwrap().0, "local.crod");
    let data = composed.compose_data().unwrap();
    assert_eq!(data.get::<u16>("port").unwrap(), 81);
    assert_eq!(data.get::<u16>("db").unwrap(), 5432);
}

#[test]
fn identities_reach_diagnostics_and_provenance() {
    let memory = Memory::new(&[("a.crod", "+port: 80,\n+bad: &nope,")]);
    let error = composer(memory)
        .load("::a")
        .unwrap()
        .compose_data()
        .unwrap_err();
    assert!(error.to_string().contains("mem:a.crod:2:"), "{error}");

    let memory = Memory::new(&[("a.crod", "+port: 80,")]);
    let data = composer(memory)
        .load("::a")
        .unwrap()
        .compose_data()
        .unwrap();
    let context = data.context(Vec::<String>::new()).unwrap();
    let steps = data.explain("port", &context).unwrap();
    let location = data.sources().location(steps[0].span).to_string();
    assert!(location.starts_with("mem:a.crod:1:"), "{location}");
}

#[test]
fn extends_goes_through_the_loader() {
    let memory = Memory::new(&[
        ("data/base/defaults.crod", "+port: 80,"),
        (
            "data/top/a.crod",
            "#![extends(super::base::defaults)]\nport: & + 1,",
        ),
        (
            "data/top/nested/b.crod",
            "#![extends(sibling)]\nport: & + 10,",
        ),
        ("data/top/nested/sibling.crod", "port: & + 100,"),
    ]);
    let asked = Arc::clone(&memory.asked);
    let composed = composer(memory).load("::data::top").unwrap();
    let names: Vec<_> = composed.layers().iter().map(CrolLayer::name).collect();
    assert_eq!(
        names,
        ["super::base::defaults", "sibling", "::data::top"],
        "extends come below, in document order"
    );
    // `super` and relative paths arrive applied to the document's directory.
    let asked = asked.lock().unwrap();
    assert_eq!(asked[1].0, ["data", "base", "defaults"]);
    assert_eq!(asked[1].1, "super::base::defaults");
    assert_eq!(asked[2].0, ["data", "top", "nested", "sibling"]);
    drop(asked);
    // `sibling` is a layer of its own and, being below `data/top`, a document
    // of the top layer too, as it would be on disk.
    let data = composed.compose_data().unwrap();
    assert_eq!(data.get::<u16>("port").unwrap(), 291);
}

#[test]
fn memory_and_the_file_system_agree() {
    let files = [
        ("data/base/defaults.crod", "+port: 80,"),
        (
            "data/top/z.crod",
            "#![extends(super::base::defaults)]\nport: & + 1,",
        ),
        (
            "data/top/a/b.crod",
            "#![extends(super::super::c)]\nport: & * 2,",
        ),
        ("data/c.crod", "+c: 1,"),
    ];
    let root = tree("loader-agree", &files);
    let layers = |composer: CrolComposer| {
        let composer = composer.load("::data::top").unwrap();
        let names: Vec<(String, Vec<String>)> = composer
            .layers()
            .iter()
            .map(|layer| {
                let documents = layer.documents().map(|(r, _, _)| r.to_owned());
                (layer.name().to_owned(), documents.collect())
            })
            .collect();
        (names, composer.compose_data().unwrap().fingerprint())
    };
    let disk = layers(CrolComposer::with_cfg(CrolConfig::<String> {
        root_path: root,
        load_extends: true,
        ..Default::default()
    }));
    assert_eq!(disk, layers(composer(Memory::new(&files))));
}

#[test]
fn a_document_is_loaded_once_and_a_cycle_is_an_error() {
    let once = Memory::new(&[
        ("base.crod", "+port: 80,"),
        ("a.crod", "#![extends(base)]\nport: & + 1,"),
        ("b.crod", "#![extends(base)]\nport: & + 1,"),
    ]);
    let composed = composer(once).load("::a").unwrap().load("::b").unwrap();
    assert_eq!(composed.layers().len(), 3);

    let cycle = Memory::new(&[
        ("a.crod", "#![extends(b)]\n+a: 1,"),
        ("b.crod", "#![extends(a)]\n+b: 1,"),
    ]);
    let error = composer(cycle).load("::a").unwrap_err();
    assert!(error.to_string().contains("cycle"), "{error}");
    assert!(error.to_string().contains("mem:"), "{error}");
}

#[test]
fn super_above_the_root_never_reaches_a_loader() {
    let memory = Memory::new(&[("a.crod", "#![extends(super::secret)]\n+a: 1,")]);
    let asked = Arc::clone(&memory.asked);
    let error = composer(memory).load("::a").unwrap_err();
    assert!(error.to_string().contains("above the root"), "{error}");
    assert_eq!(asked.lock().unwrap().len(), 1, "only `::a` was asked for");

    let memory = Memory::new(&[]);
    let asked = Arc::clone(&memory.asked);
    for path in ["super::x", "::a::..::b", "a/b", "::super::x", ""] {
        assert!(composer(memory.clone()).load(path).is_err(), "{path}");
    }
    assert!(asked.lock().unwrap().is_empty());
}

#[test]
fn a_directory_listing_cannot_leave_its_directory() {
    struct Listing;
    impl FileLoader for Listing {
        fn resolve(&self, _: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
            Ok(Some(SourceId::new("list:dir")))
        }
        fn load(&self, _: &SourceId) -> Result<Loaded, LoaderError> {
            Ok(Loaded::Directory(vec![(
                "../../secret.crod".to_owned(),
                SourceId::new("list:secret"),
            )]))
        }
    }
    let error = composer(Listing).load("::dir").unwrap_err();
    assert!(error.to_string().contains("not a relative path"), "{error}");
}

#[test]
fn falls_through_to_the_default_in_order() {
    let root = tree(
        "loader-chain",
        &[
            ("disk.crod", "+disk: 1,"),
            ("both.crod", "+from: \"disk\","),
            ("data/shared.crod", "+shared: 1,"),
        ],
    );
    let first = Memory::new(&[
        ("both.crod", "+from: \"first\","),
        ("data/top.crod", "#![extends(shared)]\n+top: 1,"),
    ]);
    let second = Memory::new(&[
        ("both.crod", "+from: \"second\","),
        ("only.crod", "+only: 1,"),
    ]);
    let data = CrolComposer::with_cfg(CrolConfig::<String> {
        root_path: root.clone(),
        load_extends: true,
        ..Default::default()
    })
    .with_file_loader(first)
    .with_file_loader(second)
    .load("::both")
    .unwrap()
    .load("::only")
    .unwrap()
    .load("::disk")
    .unwrap()
    // The document is in memory; what it extends is on disk, at the path it
    // would have below the root.
    .load("::data::top")
    .unwrap()
    .compose_data()
    .unwrap();
    assert_eq!(data.get::<String>("from").unwrap(), "first");
    assert_eq!(data.get::<u8>("only").unwrap(), 1);
    assert_eq!(data.get::<u8>("disk").unwrap(), 1);
    assert_eq!(data.get::<u8>("shared").unwrap(), 1);
}

#[test]
fn nothing_found_says_so_as_it_did() {
    let root = tree("loader-nothing", &[("a.crod", "")]);
    let config = || CrolConfig::<String> {
        root_path: root.clone(),
        ..Default::default()
    };
    // With the default loader, the message is the file system's.
    let error = CrolComposer::with_cfg(config())
        .with_file_loader(Memory::new(&[]))
        .load("::nope")
        .unwrap_err();
    assert!(error.to_string().contains("there is no"), "{error}");
    let error = CrolComposer::with_cfg(config())
        .with_file_loader(Memory::new(&[]))
        .without_file_system()
        .load("::a")
        .unwrap_err();
    assert!(
        error.to_string().contains("no file loader has it"),
        "{error}"
    );
}

#[test]
fn turning_the_file_system_off_keeps_files_out() {
    let root = tree("loader-closed", &[("secret.crod", "+secret: 1,")]);
    let memory = Memory::new(&[("a.crod", "#![extends(::secret)]\n+a: 1,")]);
    let closed = || {
        CrolComposer::with_cfg(CrolConfig::<String> {
            root_path: root.clone(),
            load_extends: true,
            ..Default::default()
        })
        .with_file_loader(memory.clone())
        .without_file_system()
    };
    assert!(closed().load("::secret").is_err());
    assert!(closed().load(root.join("secret.crod")).is_err());
    let error = closed().load("::a").unwrap_err();
    assert!(error.to_string().contains("names nothing"), "{error}");
}

#[test]
fn load_errors_carry_the_source() {
    let mut memory = Memory::new(&[("a.crod", "+a: 1,"), ("dir/b.crod", "+b: 1,")]);
    memory.failing = vec!["a.crod".to_owned(), "dir/b.crod".to_owned()];
    for (path, id) in [("::a", "mem:a.crod"), ("::dir", "mem:dir/b.crod")] {
        let error = composer(memory.clone()).load(path).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("cannot read {id}: the disk is on fire")
        );
        assert_eq!(error.source_id().unwrap().as_name(), Some(id));
    }

    struct Bytes;
    impl FileLoader for Bytes {
        fn resolve(&self, _: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
            Ok(Some(SourceId::new("bytes:x")))
        }
        fn load(&self, _: &SourceId) -> Result<Loaded, LoaderError> {
            Ok(Loaded::Document(vec![0xff]))
        }
    }
    let error = composer(Bytes).load("::x").unwrap_err();
    assert_eq!(error.to_string(), "bytes:x is not valid UTF-8");
    assert_eq!(error.source_id(), Some(&SourceId::new("bytes:x")));

    struct Broken;
    impl FileLoader for Broken {
        fn resolve(&self, _: &ModulePath<'_>) -> Result<Option<SourceId>, LoaderError> {
            Err("the index is gone".into())
        }
        fn load(&self, _: &SourceId) -> Result<Loaded, LoaderError> {
            unreachable!()
        }
    }
    // A failure stops the search: it is not "nothing here".
    let error = composer(Broken).load("::x").unwrap_err();
    assert_eq!(error.to_string(), "cannot resolve `::x`: the index is gone");
}

#[test]
fn a_second_tree_comes_before_the_default() {
    let site = tree("loader-site", &[("a.crod", "+from: \"site\",")]);
    let root = tree(
        "loader-default",
        &[("a.crod", "+from: \"default\","), ("b.crod", "+b: 1,")],
    );
    let data = CrolComposer::with_cfg(CrolConfig::<String> {
        root_path: root,
        ..Default::default()
    })
    .with_file_loader(FsLoader::new(&site))
    .load("::a")
    .unwrap()
    .load("::b")
    .unwrap()
    .compose_data()
    .unwrap();
    assert_eq!(data.get::<String>("from").unwrap(), "site");
}

#[test]
fn a_file_system_document_keeps_its_own_directory() {
    // Loaded by a path outside the root: its relative paths are its own.
    let outside = tree(
        "loader-outside",
        &[
            ("top.crod", "#![extends(below)]\nport: & + 1,"),
            ("below.crod", "+port: 80,"),
        ],
    );
    let root = tree("loader-outside-root", &[]);
    let data = CrolComposer::with_cfg(CrolConfig::<String> {
        root_path: root,
        load_extends: true,
        ..Default::default()
    })
    .with_file_loader(Memory::new(&[("below.crod", "+port: 0,")]))
    .load(outside.join("top.crod"))
    .unwrap()
    .compose_data()
    .unwrap();
    assert_eq!(data.get::<u16>("port").unwrap(), 81);
}

#[test]
fn schemas_and_project_files_read_through_loaders() {
    let schema = crol::schema::build::<crol::project::Project>()
        .to_text()
        .unwrap();
    let memory = Memory::new(&[
        ("schema.crol", &schema),
        (
            "app.crol",
            "Project { layers: List([{ name: \"base\", dir: \"data\", .. }]), .. }",
        ),
        ("data/a.crod", "+a: 1,"),
    ]);
    let loaders = FileLoaders::new().with(memory).without_file_system();
    let root = std::path::Path::new("");
    let read = loaders
        .resolve_value("::schema", root)
        .unwrap()
        .text()
        .unwrap();
    assert!(SchemaFile::from_text(&read).is_ok());

    let project = loaders
        .resolve_value("::app", root)
        .unwrap()
        .text()
        .unwrap();
    let project: crol::project::Project = crol::from_str(&project).unwrap();
    let crol::project::ProjectLayers::List(layers) = &project.layers else {
        panic!();
    };
    let layer = loaders
        .resolve(&layers[0].dir, root)
        .unwrap()
        .load(layers[0].name.clone())
        .unwrap();
    assert_eq!(layer.documents().count(), 1);
}

#[test]
fn the_composer_stays_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<CrolComposer>();
    check::<CrolConfig>();
    check::<crol::ResolvedSource>();
}

/// What a buffer holds over the files, as an editor's unsaved text.
struct Over {
    path: PathBuf,
    text: &'static str,
}

impl FileLoader for Over {
    fn resolve(&self, _: &crol::ModulePath<'_>) -> Result<Option<SourceId>, crol::LoaderError> {
        Ok(None)
    }

    fn load(&self, source: &SourceId) -> Result<Loaded, crol::LoaderError> {
        if source.canonical_path() == Some(self.path.as_path()) {
            return Ok(Loaded::Document(self.text.as_bytes().to_vec()));
        }
        FsLoader::new(".").load(source)
    }
}

fn tolerant(root: &std::path::Path, reader: Option<Over>) -> CrolComposer {
    let file_loaders = match reader {
        Some(reader) => FileLoaders::new().with_file_reader(reader),
        None => FileLoaders::new(),
    };
    CrolComposer::with_cfg(crol::CrolConfig::<String> {
        load_extends: true,
        root_path: root.to_path_buf(),
        file_loaders,
        ..Default::default()
    })
}

#[test]
fn a_file_reader_reads_every_file_however_it_is_reached() {
    let dir = tree(
        "file-reader",
        &[
            ("sub/top.crod", "#![extends(below)]\n\nport: & + 1\n"),
            ("sub/below.crod", "+port: 1\n"),
        ],
    );
    let path = dir.join("sub/below.crod").canonicalize().unwrap();
    let reader = Over {
        path,
        text: "+port: 41\n",
    };
    let loaders = FileLoaders::new().with_file_reader(reader);
    let top = dir.join("sub/top.crod");
    let layer = loaders.resolve_path(&top, &dir).load("top").unwrap();
    assert_eq!(layer.documents().count(), 1);
    let data = CrolComposer::with_cfg(crol::CrolConfig::<String> {
        load_extends: true,
        root_path: dir.clone(),
        file_loaders: loaders,
        ..Default::default()
    })
    .load_layer(layer)
    .compose_data()
    .unwrap();
    assert_eq!(data.get::<u16>("port").unwrap(), 42);
}

#[test]
fn a_tolerant_load_goes_on_past_an_extends_that_fails() {
    let dir = tree(
        "tolerant",
        &[
            (
                "top.crod",
                "#![extends(below)]\n#![extends(nothing)]\n#![extends(ring)]\n\nport: 2\n",
            ),
            ("below.crod", "#![extends(lowest)]\n\nport: 1\n"),
            ("lowest.crod", "+port: 0\n"),
            ("ring.crod", "#![extends(top)]\n"),
        ],
    );
    let mut composer = tolerant(&dir, None);
    let layer = FileLoaders::new().resolve_path(&dir.join("top.crod"), &dir);
    let loads = composer.load_layer_tolerant(layer.load("top").unwrap());

    assert_eq!(loads.len(), 3);
    assert_eq!(loads[0].path, "below");
    assert_eq!(loads[0].layers, 0..2);
    assert_eq!(loads[0].error, None);
    let text = std::fs::read_to_string(dir.join("top.crod")).unwrap();
    assert_eq!(&text[loads[1].range], "nothing");
    assert!(
        loads[1]
            .error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("nothing"),
        "{loads:?}"
    );
    assert_eq!(loads[1].layers, 2..2);
    assert!(
        loads[2]
            .error
            .as_ref()
            .unwrap()
            .to_string()
            .contains("cycle"),
        "{loads:?}"
    );

    // What could be followed lies below the layer, and it evaluates.
    let names: Vec<&str> = composer.layers().iter().map(|l| l.name()).collect();
    assert_eq!(names, ["lowest", "below", "top"]);
    let data = composer.compose_data().unwrap();
    assert_eq!(data.get::<u16>("port").unwrap(), 2);
}

#[test]
fn a_tolerant_load_follows_nothing_without_load_extends() {
    let dir = tree("tolerant-off", &[("top.crod", "#![extends(nothing)]\n")]);
    let layer = FileLoaders::new().resolve_path(&dir.join("top.crod"), &dir);
    let mut composer = CrolComposer::new();
    let loads = composer.load_layer_tolerant(layer.load("top").unwrap());
    assert!(loads.is_empty());
    assert_eq!(composer.layers().len(), 1);
}

/// A directory is read without looking at each file, but a symbolic link is
/// followed all the same.
#[cfg(unix)]
#[test]
fn a_directory_follows_symbolic_links_to_files_and_directories() {
    let root = std::env::temp_dir().join(format!("crol-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let layer = root.join("layer");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(layer.join("plain")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(layer.join("plain/a.crod"), "+a: 1\n").unwrap();
    std::fs::write(elsewhere.join("b.crod"), "+b: 2\n").unwrap();
    std::fs::write(elsewhere.join("c.crod"), "+c: 3\n").unwrap();
    std::fs::write(layer.join("picture.png"), "not data").unwrap();
    std::os::unix::fs::symlink(elsewhere.join("b.crod"), layer.join("linked.crod")).unwrap();
    std::fs::create_dir(root.join("more")).unwrap();
    std::fs::rename(elsewhere.join("c.crod"), root.join("more/c.crod")).unwrap();
    std::os::unix::fs::symlink(root.join("more"), layer.join("linked_dir")).unwrap();

    let data = crol::CrolComposer::<String>::new()
        .load_layer(crol::CrolLayer::from_path("layer", &layer).unwrap())
        .compose_data()
        .unwrap();
    assert_eq!(data.get::<i32>("a").unwrap(), 1);
    assert_eq!(data.get::<i32>("b").unwrap(), 2);
    assert_eq!(data.get::<i32>("c").unwrap(), 3);
    std::fs::remove_dir_all(&root).unwrap();
}
