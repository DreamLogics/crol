//! The composer (`SPEC.md` 7.1, 10.7).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crol::{CrolComposer, CrolConfig, CrolLayer};
use serde_derive::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
struct Service {
    image: String,
    port: u16,
    depends_on: Vec<crol::Id>,
}

#[derive(Debug, PartialEq, Deserialize)]
enum Definition {
    Service(Service),
}

/// A fresh directory with `files`.
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

fn config(root: &Path) -> CrolConfig {
    CrolConfig {
        root_path: root.to_path_buf(),
        ..Default::default()
    }
}

#[test]
fn layers_from_module_paths_and_directories() {
    let root = tree(
        "compose-layers",
        &[
            (
                "data/base/web.crod",
                "+web: Service { image: \"web\", port: 80, depends_on: [@db] },",
            ),
            (
                "data/base/nested/db.crod",
                "+db: Service { image: \"pg\", port: 5432, depends_on: [] },",
            ),
            ("data/base/.hidden.crod", "this is not read"),
            ("data/base/notes.txt", "neither is this"),
            ("data/local.crod", "web.port: 8080,"),
        ],
    );
    let all: BTreeMap<String, Definition> = CrolComposer::with_cfg(config(&root))
        .load("::data::base")
        .unwrap()
        .load(root.join("data/local.crod"))
        .unwrap()
        .compose()
        .unwrap();
    let Definition::Service(web) = &all["web"];
    assert_eq!(web.port, 8080);
    assert_eq!(all.len(), 2);
}

#[test]
fn loading_fails_only_for_sources_that_name_nothing() {
    let root = tree("compose-missing", &[("a.crod", "+a: 1 +")]);
    let error = CrolComposer::with_cfg(config(&root))
        .load("::nope")
        .unwrap_err();
    assert!(error.to_string().contains("names nothing"), "{error}");
    // A syntax error is reported by compose, not by load.
    let composer = CrolComposer::with_cfg(config(&root)).load("::a").unwrap();
    let error = composer.compose_data().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a.crod:1:8: expected an expression"),
        "{error}"
    );
}

/// The relative paths of the documents, by layer, lowest first.
fn documents(composer: &CrolComposer) -> Vec<Vec<String>> {
    composer
        .layers()
        .iter()
        .map(|layer| layer.documents().map(|d| d.0.to_owned()).collect())
        .collect()
}

#[test]
fn a_directory_extended_twice_is_loaded_once() {
    let root = tree(
        "compose-extends-directory",
        &[
            ("shared/a.crod", "+a: 1,"),
            ("shared/b.crod", "+b: 1,"),
            ("x.crod", "#![extends(shared)]\n\na: & + 1,"),
            ("y.crod", "#![extends(shared)]\n\nb: & + 1,"),
        ],
    );
    let config = CrolConfig {
        load_extends: true,
        ..config(&root)
    };
    let composer = CrolComposer::with_cfg(config)
        .load("::x")
        .unwrap()
        .load("::y")
        .unwrap();
    assert_eq!(
        documents(&composer),
        [vec!["a.crod", "b.crod"], vec!["x.crod"], vec!["y.crod"]]
    );
    let data = composer.compose_data().unwrap();
    assert_eq!(data.get::<u8>("a").unwrap(), 2);
    assert_eq!(data.get::<u8>("b").unwrap(), 2);
}

#[test]
fn a_document_of_a_directory_is_not_loaded_again_with_it() {
    let root = tree(
        "compose-extends-overlap",
        &[
            ("shared/a.crod", "+a: 1,"),
            ("shared/b.crod", "+b: 1,"),
            ("x.crod", "#![extends(shared::a)]\n\na: & + 1,"),
            ("y.crod", "#![extends(shared)]\n\nb: & + 1,"),
            // The other way around: the directory, then one of its documents.
            (
                "z.crod",
                "#![extends(shared)]\n#![extends(shared::a)]\n+z: 1,",
            ),
            // A directory with a document that extends another one of it.
            ("sibling/a.crod", "#![extends(b)]\n\nb: & + 1,"),
            ("sibling/b.crod", "+b: 1,"),
            ("w.crod", "#![extends(sibling)]\n\nb: & + 1,"),
        ],
    );
    let config = CrolConfig {
        load_extends: true,
        ..config(&root)
    };
    let composer = CrolComposer::with_cfg(config.clone())
        .load("::x")
        .unwrap()
        .load("::y")
        .unwrap();
    assert_eq!(
        documents(&composer),
        [
            vec!["a.crod"],
            vec!["x.crod"],
            vec!["b.crod"],
            vec!["y.crod"],
        ]
    );
    let data = composer.compose_data().unwrap();
    assert_eq!(data.get::<u8>("a").unwrap(), 2);
    assert_eq!(data.get::<u8>("b").unwrap(), 2);

    let composer = CrolComposer::with_cfg(config.clone()).load("::z").unwrap();
    assert_eq!(
        documents(&composer),
        [vec!["a.crod", "b.crod"], vec!["z.crod"]]
    );

    let composer = CrolComposer::with_cfg(config).load("::w").unwrap();
    assert_eq!(
        documents(&composer),
        [vec!["b.crod"], vec!["a.crod"], vec!["w.crod"]]
    );
    assert_eq!(composer.compose_data().unwrap().get::<u8>("b").unwrap(), 3);
}

#[test]
fn a_layer_that_is_named_stays_whole() {
    let root = tree(
        "compose-named-layer",
        &[
            ("named/a.crod", "#![extends(b)]\n\nb: & + 1,"),
            ("named/b.crod", "?b: 1,\nb: & + 10,"),
        ],
    );
    let config = CrolConfig {
        load_extends: true,
        ..config(&root)
    };
    let composer = CrolComposer::with_cfg(config).load("::named").unwrap();
    assert_eq!(
        documents(&composer),
        [vec!["b.crod"], vec!["a.crod", "b.crod"]]
    );
}

#[test]
fn a_directory_extending_itself_is_a_cycle() {
    let root = tree(
        "compose-extends-own-directory",
        &[("shared/a.crod", "#![extends(super::shared)]\n+a: 1,")],
    );
    let config = CrolConfig {
        load_extends: true,
        ..config(&root)
    };
    let error = CrolComposer::with_cfg(config).load("::shared").unwrap_err();
    assert!(error.to_string().contains("cycle"), "{error}");
}

#[test]
fn without_load_extends_a_layer_is_kept_as_it_is() {
    let root = tree("compose-no-extends", &[("a.crod", "+a: 1,")]);
    let composer = CrolComposer::with_cfg(config(&root))
        .load("::a")
        .unwrap()
        .load("::a")
        .unwrap();
    assert_eq!(documents(&composer).len(), 2);
}

#[test]
fn extends_loads_the_layers_below() {
    let root = tree(
        "compose-extends",
        &[
            (
                "defaults/services.crod",
                "+web: { port: 80, name: \"web\" },",
            ),
            (
                "prod/web.crod",
                "#![extends(super::defaults::services)]\n\nweb.port: 443,",
            ),
            ("cycle/a.crod", "#![extends(super::cycle::b)]\n+a: 1,"),
            ("cycle/b.crod", "#![extends(super::cycle::a)]\n+b: 1,"),
        ],
    );
    // Without `load_extends`, `extends` is ignored by the loader.
    let error = CrolComposer::with_cfg(config(&root))
        .load("::prod")
        .unwrap()
        .compose_data()
        .unwrap_err();
    assert!(
        error.to_string().contains("`web` does not exist"),
        "{error}"
    );

    let config = CrolConfig {
        load_extends: true,
        ..config(&root)
    };
    let data = CrolComposer::with_cfg(config.clone())
        .load("::prod")
        .unwrap()
        .compose_data()
        .unwrap();
    #[derive(Debug, Deserialize)]
    struct Web {
        port: u16,
        name: String,
    }
    let web: Web = data.get("web").unwrap();
    assert_eq!((web.port, web.name.as_str()), (443, "web"));

    let error = CrolComposer::with_cfg(config)
        .load("::cycle::a")
        .unwrap_err();
    assert!(error.to_string().contains("cycle"), "{error}");
}

#[test]
fn data_gives_values_warnings_and_ids() {
    let data = CrolComposer::new()
        .load_layer(CrolLayer::from_sources("base", [("a.crod", "+web: Service { image: \"w\", port: 1, depends_on: [@db] },\n+db: Service { image: \"d\", port: 2, depends_on: [] },")]))
        .load_layer(CrolLayer::from_sources("local", [("x.crod", "web.port: 10,"), ("y.crod", "web.image: \"w2\",")]))
        .compose_data()
        .unwrap();
    assert_eq!(data.keys(), ["web", "db"]);
    assert!(data.contains("db"));
    assert_eq!(data.type_name("web"), Some("Service"));
    let warnings = data.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].starts_with("warning: local/x.crod:1:1: `web` is patched from 2 documents"),
        "{}",
        warnings[0]
    );
    let (web, ids): (Service, _) = data.get_with_ids("web").unwrap();
    assert_eq!((web.port, web.image.as_str()), (10, "w2"));
    assert_eq!(ids[0].rust_path, "web.depends_on[0]");
}

#[test]
fn deserialization_errors_name_the_root_key() {
    let data = CrolComposer::new()
        .load_layer(CrolLayer::from_sources(
            "base",
            [(
                "a.crod",
                "+web: Service { image: \"w\", port: -1, depends_on: [] },",
            )],
        ))
        .compose_data()
        .unwrap();
    let error = data.get::<Service>("web").unwrap_err();
    assert_eq!(
        error.to_string(),
        "expected u16, found -1 (base/a.crod:1:35, while deserializing web.port)"
    );
}

const SERVICES: &str = r#"
+database: Service { image: "pg", port: 5432, depends_on: [] },
+web: Service { image: "web", port: 80, depends_on: [@database] },
<prod> { web.port: 443 },
<prod_eu> { web.image: "web-eu" },
<cache> { web.depends_on: & + [@cache] },
"#;

fn spec_variants() -> crol::CrolVariants {
    crol::CrolVariants::new()
        .add_grouped(["dev", "prod", "prod_eu"])
        .add_discrete("cache")
        .add_parent("prod_eu", "prod")
}

#[test]
fn variants_and_contexts() {
    // The API sketch of SPEC.md 10.7.
    let config = CrolConfig {
        variants: spec_variants(),
        ..Default::default()
    };
    let web: BTreeMap<String, Definition> = CrolComposer::with_cfg(config.clone())
        .load_layer(CrolLayer::from_sources("base", [("a.crod", SERVICES)]))
        .with_variant("prod_eu")
        .unwrap()
        .with_variant("cache")
        .unwrap()
        .compose()
        .unwrap();
    let Definition::Service(web) = &web["web"];
    assert_eq!((web.port, web.image.as_str()), (443, "web-eu"));
    assert_eq!(web.depends_on.len(), 2);

    let data = CrolComposer::with_cfg(config.clone())
        .load_layer(CrolLayer::from_sources("base", [("a.crod", SERVICES)]))
        .compose_data()
        .unwrap();
    assert_eq!(data.contexts().count(), 8);
    let prod = data.context(["prod"]).unwrap();
    let in_prod: Service = data.get_in("web", &prod).unwrap();
    assert_eq!(in_prod.port, 443);
    let base: Service = data.get("web").unwrap();
    assert_eq!(base.port, 80);
    assert!(data.context(["dev", "prod"]).is_err());
    let fingerprint = data.fingerprint();
    // Evaluating again gives the same fingerprint.
    let again = CrolComposer::with_cfg(config.clone())
        .load_layer(CrolLayer::from_sources("base", [("a.crod", SERVICES)]))
        .compose_data()
        .unwrap();
    assert_eq!(again.fingerprint(), fingerprint);

    // `with_variant` checks what it is given.
    let error = CrolComposer::with_cfg(config.clone())
        .with_variant("staging")
        .unwrap_err();
    assert!(
        error.to_string().contains("not a declared variant"),
        "{error}"
    );
    let error = CrolComposer::with_cfg(config)
        .with_variant("dev")
        .unwrap()
        .with_variant("prod")
        .unwrap_err();
    assert!(error.to_string().contains("at most one"), "{error}");
}

#[test]
fn typed_variants() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Env {
        Dev,
        Prod,
        ProdEu,
        Cache,
    }

    impl crol::VariantName for Env {
        fn name(&self) -> &str {
            match self {
                Env::Dev => "dev",
                Env::Prod => "prod",
                Env::ProdEu => "prod_eu",
                Env::Cache => "cache",
            }
        }

        fn from_name(name: &str) -> Option<Self> {
            Some(match name {
                "dev" => Env::Dev,
                "prod" => Env::Prod,
                "prod_eu" => Env::ProdEu,
                "cache" => Env::Cache,
                _ => return None,
            })
        }
    }

    let variants = crol::CrolVariants::<Env>::default()
        .add_grouped([Env::Dev, Env::Prod, Env::ProdEu])
        .add_discrete(Env::Cache)
        .add_parent(Env::ProdEu, Env::Prod);
    let data = CrolComposer::with_cfg(CrolConfig {
        variants,
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources("base", [("a.crod", SERVICES)]))
    .with_variant(Env::ProdEu)
    .unwrap()
    .compose_data()
    .unwrap();
    let web: Service = data.get("web").unwrap();
    assert_eq!(web.image, "web-eu");
    let context = data.context([Env::Dev, Env::Cache]).unwrap();
    assert_eq!(data.context_names(&context), ["cache", "dev"]);
}

#[test]
fn explain_lists_the_operations_behind_a_value() {
    let config = CrolConfig {
        variants: crol::CrolVariants::new().add_grouped(["dev", "prod"]),
        ..Default::default()
    };
    let base = CrolLayer::from_sources(
        "base",
        [(
            "services.crod",
            "+web: { limits: { memory_mb: 512 }, port: 80 },\n<prod> { web.limits.memory_mb: & * 4 },",
        )],
    );
    let local = CrolLayer::from_sources(
        "local",
        [("a.crod", "web.limits.memory_mb: 1024,\nweb.port: 81,")],
    );
    let data = CrolComposer::with_cfg(config)
        .load_layer(base)
        .load_layer(local)
        .compose_data()
        .unwrap();
    let prod = data.context(["prod"]).unwrap();
    let steps = data.explain("web.limits.memory_mb", &prod).unwrap();
    let summary: Vec<(String, String, Option<String>)> = steps
        .iter()
        .map(|s| {
            (
                s.layer.clone(),
                s.entry.clone(),
                s.value.as_ref().map(crol_eval::print::to_text),
            )
        })
        .collect();
    // `web.port: 81` does not touch the path, so it is not listed.
    assert_eq!(
        summary,
        [
            ("base".into(), "+web".into(), Some("512".into())),
            (
                "local".into(),
                "web.limits.memory_mb".into(),
                Some("1024".into())
            ),
            (
                "base".into(),
                "web<prod>.limits.memory_mb".into(),
                Some("4096".into())
            ),
        ]
    );
    assert_eq!(steps[0].kind, crol::StepKind::Definition);
    assert_eq!(steps[1].kind, crol::StepKind::Patch);
}

const CIVS_AND_RULES: &str = "
+unit: { hp: 10, speed: 1 }
<empire> {
    unit.hp: & + 5,
}
<rebel> {
    unit.hp: & + 1,
}
<fast> {
    unit.speed: & * 2,
}
<broken & rebel> {
    unit.hp: & / 0,
}
";

fn civs_and_rules(contexts: Option<Vec<Vec<String>>>, each_variant: bool) -> crol::CrolComposer {
    let variants = crol::CrolVariants::new()
        .add_grouped(["empire", "rebel"])
        .add_discrete("fast")
        .add_discrete("broken");
    crol::CrolComposer::with_cfg(crol::CrolConfig {
        variants,
        contexts,
        each_variant,
        ..Default::default()
    })
    .load_layer(crol::CrolLayer::from_sources(
        "base",
        [("a.crod", CIVS_AND_RULES)],
    ))
}

#[derive(Debug, PartialEq, serde_derive::Deserialize)]
struct Stats {
    hp: u32,
    speed: u32,
}

#[test]
fn a_context_can_be_evaluated_later() {
    let mut data = civs_and_rules(Some(Vec::new()), false)
        .compose_data()
        .unwrap();
    assert!(data.context(["empire", "fast"]).is_err());
    let context = data.evaluate(["empire", "fast"]).unwrap();
    assert_eq!(
        data.get_in::<Stats>("unit", &context).unwrap(),
        Stats { hp: 15, speed: 2 }
    );
    assert!(data.context(["empire", "fast"]).is_ok());
}

#[test]
fn evaluating_a_context_again_changes_nothing() {
    let mut data = civs_and_rules(Some(Vec::new()), false)
        .compose_data()
        .unwrap();
    data.evaluate(["rebel"]).unwrap();
    let fingerprint = data.fingerprint();
    data.evaluate(["rebel"]).unwrap();
    assert_eq!(data.fingerprint(), fingerprint);
    assert_eq!(data.contexts().count(), 2);
}

#[test]
fn a_context_evaluated_later_fingerprints_as_one_evaluated_at_once() {
    let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let at_once = civs_and_rules(Some(vec![names(&["rebel"]), names(&["fast"])]), false)
        .compose_data()
        .unwrap();
    let mut later = civs_and_rules(Some(Vec::new()), false)
        .compose_data()
        .unwrap();
    later.evaluate(["fast"]).unwrap();
    later.evaluate(["rebel"]).unwrap();
    assert_eq!(at_once.fingerprint(), later.fingerprint());
}

#[test]
fn an_error_in_a_context_evaluated_later_leaves_the_data_as_it_was() {
    let mut data = civs_and_rules(Some(Vec::new()), false)
        .compose_data()
        .unwrap();
    let error = data.evaluate(["rebel", "broken"]).unwrap_err();
    assert!(error.to_string().contains("division by zero"), "{error}");
    assert_eq!(data.contexts().count(), 1);
}

#[test]
fn each_variant_evaluates_one_context_for_each() {
    let data = civs_and_rules(None, true).compose_data().unwrap();
    // The base, `empire`, `rebel`, `fast` and `broken`; the error needs two.
    assert_eq!(data.contexts().count(), 5);
}

#[test]
fn every_combination_is_the_default() {
    assert!(civs_and_rules(None, false).compose_data().is_err());
}

#[test]
fn a_context_evaluated_later_reports_its_constraints_and_reads_lazily() {
    // Only what the context needs of the base is evaluated again.
    let text = "+a: { hp: 1 }\n+b: { hp: &root.a.hp + 1 }\n+c: { hp: 3 }\nb<rebel>.hp: & * 10";
    let variants = crol::CrolVariants::new().add_grouped(["rebel", "empire"]);
    let mut data = crol::CrolComposer::with_cfg(crol::CrolConfig {
        variants,
        contexts: Some(Vec::new()),
        ..Default::default()
    })
    .load_layer(crol::CrolLayer::from_sources("base", [("a.crod", text)]))
    .compose_data()
    .unwrap();
    let rebel = data.evaluate(["rebel"]).unwrap();
    let hp = |data: &crol::CrolData, key: &str, context| {
        data.get_in::<std::collections::BTreeMap<String, u32>>(key, context)
            .unwrap()["hp"]
    };
    assert_eq!(hp(&data, "b", &rebel), 20);
    assert_eq!(hp(&data, "c", &rebel), 3);
}

#[test]
fn data_can_move_between_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<crol::CrolData>();
}

fn reloadable() -> crol::CrolData {
    let base = crol::CrolLayer::from_sources(
        "base",
        [
            (
                "units.crod",
                "+a: { hp: 1 }\n+b: { hp: &root.a.hp + 1 }\n+c: { hp: 9 }",
            ),
            ("other.crod", "+d: { hp: 1 }"),
        ],
    );
    let top = crol::CrolLayer::from_sources("mod", [("patch.crod", "c.hp: 10")]);
    let variants = crol::CrolVariants::new().add_grouped(["rebel", "empire"]);
    crol::CrolComposer::with_cfg(crol::CrolConfig {
        variants,
        ..Default::default()
    })
    .load_layer(base)
    .load_layer(top)
    .compose_data()
    .unwrap()
}

fn change<'a>(layer: &'a str, path: &'a str, text: Option<&'a str>) -> crol::DocumentChange<'a> {
    crol::DocumentChange { layer, path, text }
}

#[test]
fn key_fingerprints_say_which_keys_differ() {
    use crol::KeyChange::{Added, Changed, Removed};
    let one = reloadable();
    let mut two = reloadable();
    assert_eq!(one.key_fingerprints(), two.key_fingerprints());
    two.reload(&[change(
        "mod",
        "patch.crod",
        Some("a.hp: 5\n-d\n+e: { hp: 1 }"),
    )])
    .unwrap();
    assert_eq!(
        crol::changed_keys(&one.key_fingerprints(), &two.key_fingerprints()),
        [
            Changed("a".to_owned()),
            Changed("b".to_owned()),
            Changed("c".to_owned()),
            Removed("d".to_owned()),
            Added("e".to_owned()),
        ]
    );
}

#[test]
fn a_key_fingerprint_sees_a_change_in_one_context() {
    let one = reloadable();
    let mut two = reloadable();
    two.reload(&[change(
        "mod",
        "patch.crod",
        Some("c.hp: 10\nd<rebel>.hp: 2"),
    )])
    .unwrap();
    let changed = crol::changed_keys(&one.key_fingerprints(), &two.key_fingerprints());
    assert_eq!(changed, [crol::KeyChange::Changed("d".to_owned())]);
}

#[test]
fn reloading_reports_what_changed_and_keeps_the_contexts() {
    let mut data = reloadable();
    let contexts = data.contexts().count();
    let reloaded = data
        .reload(&[change(
            "base",
            "units.crod",
            Some("+a: { hp: 3 }\n+b: { hp: &root.a.hp + 1 }\n+c: { hp: 9 }"),
        )])
        .unwrap();
    let changed: Vec<&str> = reloaded.changed.iter().map(|c| c.key()).collect();
    assert_eq!(changed, ["a", "b"]);
    assert_eq!(data.contexts().count(), contexts);
    assert_eq!(
        data.get::<std::collections::BTreeMap<String, u32>>("b")
            .unwrap()["hp"],
        4
    );
}

#[test]
fn reloading_adds_and_removes_documents() {
    let mut data = reloadable();
    data.reload(&[change("mod", "new.crod", Some("+z: { hp: 1 }"))])
        .unwrap();
    assert!(data.contains("z"));
    data.reload(&[change("mod", "new.crod", None)]).unwrap();
    assert!(!data.contains("z"));
}

#[test]
fn data_that_does_not_load_leaves_it_as_it_was() {
    let mut data = reloadable();
    let fingerprint = data.fingerprint();
    for text in ["a.hp: ", "nothing.hp: 1", "a.hp: 1 / 0"] {
        assert!(
            data.reload(&[change("mod", "patch.crod", Some(text))])
                .is_err()
        );
        assert_eq!(data.fingerprint(), fingerprint);
    }
    assert!(
        data.reload(&[change("nowhere", "a.crod", Some(""))])
            .is_err()
    );
    assert!(data.reload(&[change("mod", "absent.crod", None)]).is_err());
}

#[test]
fn a_context_evaluated_later_stays_after_reloading() {
    let mut data = civs_and_rules(Some(Vec::new()), false)
        .compose_data()
        .unwrap();
    let context = data.evaluate(["empire", "fast"]).unwrap();
    data.reload(&[change(
        "base",
        "a.crod",
        Some("+unit: { hp: 20, speed: 1 }\nunit<fast>.speed: 3"),
    )])
    .unwrap();
    assert_eq!(
        data.get_in::<Stats>("unit", &context).unwrap(),
        Stats { hp: 20, speed: 3 }
    );
}

const COMPILED: &str = "shared := { hp: 1 }\n+a: { ..&shared }\n+b: { hp: &root.a.hp + 1 }\n";

fn compiled_layer() -> crol::CrolLayer {
    let text = crol::CrolLayer::from_sources("base", [("units.crod", COMPILED)]);
    crol::CrolLayer::from_binary("base", &text.to_binary().unwrap()).unwrap()
}

#[test]
fn a_compiled_layer_loads_as_its_text_does() {
    let text = crol::CrolLayer::from_sources("base", [("units.crod", COMPILED)]);
    let one = crol::CrolComposer::<String>::new()
        .load_layer(text)
        .compose_data()
        .unwrap();
    let two = crol::CrolComposer::<String>::new()
        .load_layer(compiled_layer())
        .compose_data()
        .unwrap();
    assert_eq!(one.fingerprint(), two.fingerprint());
}

#[test]
fn a_text_layer_goes_on_a_compiled_one_and_sees_its_templates() {
    let top = crol::CrolLayer::from_sources("mod", [("a.crod", "a.hp: 5\n+c: { ..&shared }")]);
    let data = crol::CrolComposer::<String>::new()
        .load_layer(compiled_layer())
        .load_layer(top)
        .compose_data()
        .unwrap();
    let hp = |key| {
        data.get::<std::collections::BTreeMap<String, u32>>(key)
            .unwrap()["hp"]
    };
    assert_eq!((hp("a"), hp("b"), hp("c")), (5, 6, 1));
}

#[test]
fn a_template_defined_twice_across_a_compiled_layer_is_an_error() {
    let top = crol::CrolLayer::from_sources("mod", [("a.crod", "shared := { hp: 2 }")]);
    let error = crol::CrolComposer::<String>::new()
        .load_layer(compiled_layer())
        .load_layer(top)
        .compose_data()
        .unwrap_err();
    assert!(error.to_string().contains("defined twice"), "{error}");
}

#[test]
fn errors_in_a_compiled_layer_point_into_its_text() {
    let top = crol::CrolLayer::from_sources("mod", [("a.crod", "+a: { hp: 2 }")]);
    let error = crol::CrolComposer::<String>::new()
        .load_layer(compiled_layer())
        .load_layer(top)
        .compose_data()
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[test]
fn explain_works_through_a_compiled_layer() {
    let top = crol::CrolLayer::from_sources("mod", [("a.crod", "a.hp: 5")]);
    let data = crol::CrolComposer::<String>::new()
        .load_layer(compiled_layer())
        .load_layer(top)
        .compose_data()
        .unwrap();
    let context = data.context(Vec::<String>::new()).unwrap();
    assert_eq!(data.explain("a.hp", &context).unwrap().len(), 2);
}

#[test]
fn reloading_a_document_of_a_compiled_layer_uses_its_text() {
    let mut data = crol::CrolComposer::<String>::new()
        .load_layer(compiled_layer())
        .compose_data()
        .unwrap();
    let reloaded = data
        .reload(&[change(
            "base",
            "units.crod",
            Some("+a: { hp: 8 }\n+b: { hp: 1 }"),
        )])
        .unwrap();
    assert!(!reloaded.changed.is_empty());
    assert_eq!(
        data.get::<std::collections::BTreeMap<String, u32>>("a")
            .unwrap()["hp"],
        8
    );
}

#[test]
fn a_layer_using_a_template_of_another_does_not_compile_alone() {
    let layer = crol::CrolLayer::from_sources("top", [("a.crod", "+c: { ..&elsewhere }")]);
    assert!(layer.to_binary().is_err());
}

#[test]
fn a_compiled_file_is_read_as_a_layer_and_not_in_a_directory() {
    let dir = std::env::temp_dir().join(format!("crol-crob-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let text = crol::CrolLayer::from_sources("base", [("units.crod", COMPILED)]);
    let file = dir.join("base.crob");
    std::fs::write(&file, text.to_binary().unwrap()).unwrap();
    let data = crol::CrolComposer::<String>::new()
        .load(file.as_path())
        .unwrap()
        .compose_data()
        .unwrap();
    assert!(data.contains("a"));
    let error = crol::CrolLayer::from_path("dir", &dir).unwrap_err();
    assert!(error.to_string().contains("a layer of its own"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}
