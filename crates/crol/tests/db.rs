//! `CrolDb`: typed paths, transactions and the active layer (`SPEC.md` 10.14).

#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crol::{CrolComposer, CrolConfig, CrolDb, CrolLayer, CrolVariants, KeyChange, Navigable as _};
use crol_derive::{Navigable, Schema, croxp};
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
struct Limits {
    cpu: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
struct Options {
    a: bool,
    port: i64,
    limits: Limits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
struct Item {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
struct Tree {
    value: i64,
    child: Option<Box<Tree>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
#[serde(rename_all = "kebab-case")]
struct Config {
    retries: i64,
    url: String,
    options: Options,
    items: Vec<Item>,
    named: BTreeMap<String, i64>,
    pair: (i64, String),
    tree: Tree,
    the_mode: Mode,
    #[crol(leaf)]
    #[serde(rename = "odd key")]
    odd: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
enum Mode {
    Fast,
    Slow,
}

const BASE: &str = r#"
+retries: 1
+url: "http://web:" + &root.options.port
+options: { a: false, port: 80, limits: { cpu: 1 } }
+items: [{ name: "one" }, { name: "two" }]
+named: { a: 1, b: 2 }
+pair: (1, "x")
+tree: { value: 1, child: Some({ value: 2, child: None }) }
+"the-mode": Fast
+"odd key": 7
<prod> {
    retries: & * 4
}
<prod & cache> {
    options.port: 99
}
"#;

type Db = CrolDb<Config>;

fn composer() -> CrolComposer {
    CrolComposer::with_cfg(CrolConfig {
        variants: CrolVariants::new()
            .add_grouped(["prod", "dev"])
            .add_discrete("cache"),
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources("base", [("a.crod", BASE)]))
}

fn db() -> Db {
    composer().compose_db().unwrap()
}

/// A directory of its own, removed at the end of the test.
struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("crol-db-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(file: &Path) -> Db {
    composer().load_active(file).unwrap().compose_db().unwrap()
}

#[test]
fn paths_read_every_kind_of_step() {
    let db = db();
    let select = db.select();
    let path = Config::path;
    assert_eq!(select.get(path().retries()).unwrap(), 1);
    assert_eq!(select.get(path().options().limits().cpu()).unwrap(), 1);
    assert_eq!(select.get(path().items().at(1).name()).unwrap(), "two");
    assert_eq!(select.get(path().named().key("b")).unwrap(), 2);
    assert_eq!(select.get(path().pair().t1()).unwrap(), "x");
    let child = path().tree().child().some().value();
    assert_eq!(select.get(child).unwrap(), 2);
    assert_eq!(select.get(path().the_mode()).unwrap(), Mode::Fast);
    assert_eq!(select.get(path().odd()).unwrap(), 7);
    assert_eq!(
        select.get(path().options().limits()).unwrap(),
        Limits { cpu: 1 }
    );
}

#[test]
fn a_path_that_is_not_there_is_an_error_where_it_is_used() {
    let db = db();
    let select = db.select();
    let error = select.get(Config::path().items().at(5).name());
    assert_eq!(
        error.unwrap_err().to_string(),
        "`items[5].name` is not set in this context"
    );
    let none = Config::path().tree().child().some().child().some().value();
    assert!(select.get(none).is_err());
}

#[test]
fn select_reads_a_context() {
    let db = db();
    let select = db.select().with_variant("prod").unwrap();
    assert_eq!(select.get(Config::path().retries()).unwrap(), 4);
    assert!(db.select().with_variant("nope").is_err());
}

#[test]
fn select_needs_an_evaluated_context() {
    let mut db: Db = CrolComposer::with_cfg(CrolConfig {
        variants: CrolVariants::new()
            .add_grouped(["prod", "dev"])
            .add_discrete("cache"),
        contexts: Some(Vec::new()),
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources("base", [("a.crod", BASE)]))
    .compose_db()
    .unwrap();
    let port = Config::path().options().port();
    let select = db.select().with_variant("prod").unwrap();
    let select = select.with_variant("cache").unwrap();
    assert!(
        select
            .get(&port)
            .unwrap_err()
            .to_string()
            .contains("evaluated")
    );
    db.evaluate(["prod", "cache"]).unwrap();
    let select = db.select().with_variant("prod").unwrap();
    let select = select.with_variant("cache").unwrap();
    assert_eq!(select.get(&port).unwrap(), 99);
}

#[test]
fn a_commit_says_what_changed_and_late_references_follow() {
    let mut db = db();
    let reloaded = db
        .transaction()
        .at(Config::path().options().port())
        .set(8080)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(
        reloaded.changed,
        [
            KeyChange::Changed("options".to_owned()),
            KeyChange::Changed("url".to_owned()),
        ]
    );
    let url = db.select().get(Config::path().url()).unwrap();
    assert_eq!(url, "http://web:8080");
}

#[test]
fn nothing_changes_before_commit() {
    let mut db = db();
    let retries = Config::path().retries();
    let tx = db.transaction().at(&retries).set(5).unwrap();
    drop(tx);
    assert_eq!(db.select().get(&retries).unwrap(), 1);
}

#[test]
fn reads_see_the_transaction_so_far() {
    let mut db = db();
    let retries = Config::path().retries();
    let mut tx = db
        .transaction()
        .at(&retries)
        .set(5)
        .unwrap()
        .at(&retries)
        .update(|v| v + 1)
        .unwrap();
    assert_eq!(tx.get(&retries).unwrap(), 6);
    let mut tx = tx.at(&retries).set_expr(croxp! { & * 2 }).unwrap();
    assert_eq!(tx.get(&retries).unwrap(), 12);
    tx.commit().unwrap();
    assert_eq!(db.select().get(&retries).unwrap(), 12);
}

#[test]
fn scopes_nest_and_their_paths_are_relative() {
    let mut db = db();
    db.transaction()
        .within(Config::path().options(), |o| {
            o.at(Options::path().port())
                .update(|p| p + 1)?
                .within(Options::path().limits(), |l| {
                    l.at(Limits::path().cpu()).set(2)
                })?
                .at(Options::path().a())
                .set_expr(croxp! { !& })
        })
        .unwrap()
        .at(Config::path().retries())
        .set(9)
        .unwrap()
        .commit()
        .unwrap();
    let options = db.select().get(Config::path().options()).unwrap();
    assert_eq!(
        options,
        Options {
            a: true,
            port: 81,
            limits: Limits { cpu: 2 },
        }
    );
    assert_eq!(db.select().get(Config::path().retries()).unwrap(), 9);
}

#[test]
fn an_error_in_a_scope_ends_the_transaction() {
    let mut db = db();
    let result = db.transaction().within(Config::path().items(), |items| {
        let items = items.at(Vec::<Item>::path().at(9).name());
        items.update(|name| name + "!")
    });
    assert!(result.is_err());
    assert_eq!(db.select().get(Config::path().retries()).unwrap(), 1);
}

#[test]
fn a_failed_commit_leaves_the_data() {
    let mut db = db();
    let port = Config::path().options().port();
    let result = db
        .transaction()
        .at(&port)
        .set_expr(croxp! { & + "x" ~ 1 })
        .unwrap()
        .commit();
    assert!(result.is_err());
    assert_eq!(db.select().get(&port).unwrap(), 80);
    db.transaction().at(&port).set(1).unwrap().commit().unwrap();
    assert_eq!(db.select().get(&port).unwrap(), 1);
}

#[test]
fn a_write_applies_to_the_whole_context() {
    let mut db = db();
    db.evaluate(["prod", "cache"]).unwrap();
    let port = Config::path().options().port();
    db.transaction()
        .with_variant("prod")
        .unwrap()
        .with_variant("cache")
        .unwrap()
        .at(&port)
        .set(7)
        .unwrap()
        .commit()
        .unwrap();
    let in_context = |variants: &[&str]| {
        let mut select = db.select();
        for variant in variants {
            select = select.with_variant(*variant).unwrap();
        }
        select.get(&port).unwrap()
    };
    assert_eq!(in_context(&["prod", "cache"]), 7);
    assert_eq!(in_context(&["prod"]), 80);
    assert_eq!(in_context(&[]), 80);
}

#[test]
fn a_base_write_reaches_contexts_unless_something_more_specific_applies() {
    let mut db = db();
    db.evaluate(["prod", "cache"]).unwrap();
    let retries = Config::path().retries();
    let port = Config::path().options().port();
    db.transaction()
        .at(&retries)
        .set(5)
        .unwrap()
        .at(&port)
        .set(1)
        .unwrap()
        .commit()
        .unwrap();
    let prod = db.select().with_variant("prod").unwrap();
    assert_eq!(prod.get(&retries).unwrap(), 20);
    assert_eq!(prod.get(&port).unwrap(), 1);
    let both = prod.with_variant("cache").unwrap();
    assert_eq!(both.get(&port).unwrap(), 99);
}

#[test]
fn a_write_that_does_not_take_effect_is_an_error() {
    let base = CrolLayer::from_sources("base", [("a.crod", "+x: 1\n<!cache> {\n    x: 2\n}\n")]);
    let config = CrolConfig {
        variants: CrolVariants::new().add_discrete("cache"),
        ..Default::default()
    };
    let mut db: CrolDb<BTreeMap<String, i64>> = CrolComposer::with_cfg(config)
        .load_layer(base)
        .compose_db()
        .unwrap();
    let x = || BTreeMap::<String, i64>::path().key("x");
    let error = db
        .transaction()
        .at(x())
        .set(5)
        .unwrap()
        .commit()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "the write to `x` did not take effect: `x<!cache>` in the layer `base` decides it"
    );
    let error = db
        .transaction()
        .at(x())
        .set_expr(croxp! { &root.y ?? 3 })
        .unwrap()
        .commit()
        .unwrap_err();
    assert!(error.to_string().contains("did not take effect"), "{error}");
}

#[test]
fn an_expression_takes_effect_under_an_operation_that_uses_it() {
    let base =
        CrolLayer::from_sources("base", [("a.crod", "+x: 1\n<!cache> {\n    x: & * 2\n}\n")]);
    let config = CrolConfig {
        variants: CrolVariants::new().add_discrete("cache"),
        ..Default::default()
    };
    let mut db: CrolDb<BTreeMap<String, i64>> = CrolComposer::with_cfg(config)
        .load_layer(base)
        .compose_db()
        .unwrap();
    let x = BTreeMap::<String, i64>::path().key("x");
    let tx = db.transaction().at(&x).set_expr(croxp! { & + 1 }).unwrap();
    tx.commit().unwrap();
    assert_eq!(db.select().get(&x).unwrap(), 4);
}

#[test]
fn set_replaces_a_block() {
    let mut db = db();
    let named = Config::path().named();
    let new = BTreeMap::from([("c".to_owned(), 3)]);
    db.transaction()
        .at(&named)
        .set(new.clone())
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&named).unwrap(), new);
}

#[test]
fn expressions_can_hold_rust_values() {
    let mut db = db();
    let retries = Config::path().retries();
    let delta = 10;
    db.transaction()
        .at(&retries)
        .set_expr(croxp! { & + #delta })
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&retries).unwrap(), 11);
}

#[test]
fn croxp_keeps_the_text() {
    assert_eq!(croxp! { & + 2 }.as_str(), "& + 2");
    assert_eq!(croxp! { !& }.as_str(), "!&");
    assert_eq!(
        croxp! { &root.db<dev & !cache>.port + 1 }.as_str(),
        "&root.db<dev & !cache>.port + 1"
    );
    assert_eq!(
        croxp! { & ~ { a: 1, b: [@web] } }.as_str(),
        "& ~ { a: 1, b: [@web] }"
    );
}

#[test]
fn removing_a_key() {
    let mut db = db();
    let b = || Config::path().named().key("b");
    db.transaction().at(b()).remove().unwrap().commit().unwrap();
    assert!(db.select().get(b()).is_err());
}

#[test]
fn removing_what_only_this_layer_set_leaves_nothing() {
    let directory = Directory::new("remove");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    let c = || Config::path().named().key("c");
    let tx = db.transaction().at(c()).set(3).unwrap();
    let tx = tx.at(c()).remove().unwrap();
    tx.commit().unwrap();
    db.save().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
    // A plain remove of what nothing set stays an error.
    let tx = db.transaction().at(c()).remove().unwrap();
    assert!(tx.commit().is_err());
}

#[test]
fn removing_what_the_layers_below_set_stays() {
    let directory = Directory::new("remove-below");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    let b = || Config::path().named().key("b");
    let tx = db.transaction().at(b()).set(3).unwrap();
    tx.at(b()).remove().unwrap().commit().unwrap();
    db.save().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "-named.b\n");
}

#[test]
fn the_active_layer_is_written_and_read_back() {
    let directory = Directory::new("round-trip");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    let retries = Config::path().retries();
    let port = Config::path().options().port();
    db.transaction()
        .at(&retries)
        .set_expr(croxp! { & + 2 })
        .unwrap()
        .at(&retries)
        .set_expr(croxp! { & + 2 })
        .unwrap()
        .at(&port)
        .set(1)
        .unwrap()
        .at(&port)
        .set(2)
        .unwrap()
        .at(Config::path().odd())
        .set(8)
        .unwrap()
        .commit()
        .unwrap();
    db.transaction()
        .with_variant("prod")
        .unwrap()
        .at(&port)
        .set(3)
        .unwrap()
        .commit()
        .unwrap();
    db.save().unwrap();
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        written,
        "retries: & + 2\nretries: & + 2\n^options.port: 2\n^\"odd key\": 8\n\
         <prod> {\n    ^options.port: 3,\n}\n"
    );

    let mut again = open(&file);
    assert_eq!(again.select().get(&retries).unwrap(), 5);
    assert_eq!(again.select().get(&port).unwrap(), 2);
    let prod = again.select().with_variant("prod").unwrap();
    assert_eq!(prod.get(&port).unwrap(), 3);
    // What was read goes on: this adds to the list, and replaces nothing.
    let tx = again.transaction().at(&retries).set_expr(croxp! { & + 1 });
    tx.unwrap().commit().unwrap();
    again.save().unwrap();
    assert_eq!(open(&file).select().get(&retries).unwrap(), 6);
}

#[test]
fn an_expression_stays_relative_to_the_layers_below() {
    let directory = Directory::new("relative");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    let retries = Config::path().retries();
    let tx = db.transaction().at(&retries).set_expr(croxp! { & + 2 });
    tx.unwrap().commit().unwrap();
    db.save().unwrap();
    let base = BASE.replace("+retries: 1", "+retries: 10");
    let db: Db = CrolComposer::with_cfg(CrolConfig {
        variants: CrolVariants::new()
            .add_grouped(["prod", "dev"])
            .add_discrete("cache"),
        ..Default::default()
    })
    .load_layer(CrolLayer::from_sources("base", [("a.crod", base)]))
    .load_active(&file)
    .unwrap()
    .compose_db()
    .unwrap();
    assert_eq!(db.select().get(&retries).unwrap(), 12);
}

#[test]
fn a_plain_set_clears_what_it_replaces_at_the_root_too() {
    let directory = Directory::new("clear");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    let retries = Config::path().retries();
    let options = Config::path().options();
    let value = Options {
        a: true,
        port: 5,
        limits: Limits { cpu: 5 },
    };
    db.transaction()
        .at(&retries)
        .set_expr(croxp! { & + 2 })
        .unwrap()
        .at(&retries)
        .set(5)
        .unwrap()
        .at(Config::path().options().port())
        .set(1)
        .unwrap()
        .at(&options)
        .set(value.clone())
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&retries).unwrap(), 5);
    assert_eq!(db.select().get(&options).unwrap(), value);
    db.save().unwrap();
    assert_eq!(open(&file).select().get(&retries).unwrap(), 5);
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        !written.contains("& + 2") && !written.contains("options.port"),
        "{written}"
    );
}

#[test]
fn an_expression_that_does_not_use_the_value_below_clears_too() {
    let mut db = db();
    let retries = Config::path().retries();
    let tx = db
        .transaction()
        .at(&retries)
        .set_expr(croxp! { & + 2 })
        .unwrap();
    let tx = tx
        .at(&retries)
        .set_expr(croxp! { &root.options.port })
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(db.select().get(&retries).unwrap(), 80);
}

#[test]
fn a_missing_active_layer_is_empty_and_save_creates_it() {
    let directory = Directory::new("first-run");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    assert!(!file.exists());
    db.save().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
}

#[test]
fn only_what_was_committed_is_saved() {
    let directory = Directory::new("committed");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    drop(
        db.transaction()
            .at(Config::path().retries())
            .set(5)
            .unwrap(),
    );
    db.save().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
}

#[test]
fn save_refuses_a_file_that_changed() {
    let directory = Directory::new("changed");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    std::fs::write(&file, "retries: 3\n").unwrap();
    let error = db.save().unwrap_err();
    assert!(error.to_string().contains("changed since it was read"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "retries: 3\n");
}

#[test]
fn without_an_active_layer_transactions_work_and_save_is_an_error() {
    let directory = Directory::new("memory");
    let mut db = db();
    let retries = Config::path().retries();
    db.transaction()
        .at(&retries)
        .set(5)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&retries).unwrap(), 5);
    assert!(db.save().unwrap_err().to_string().contains("save_as"));

    let file = directory.file("state.crod");
    db.save_as(&file).unwrap();
    assert!(db.save_as(&file).is_err());
    db.transaction()
        .at(&retries)
        .set(6)
        .unwrap()
        .commit()
        .unwrap();
    db.save().unwrap();
    assert_eq!(open(&file).select().get(&retries).unwrap(), 6);
}

#[test]
fn the_active_layer_is_the_last_and_only_one() {
    let directory = Directory::new("last");
    let file = directory.file("user.crod");
    let composer = composer().load_active(&file).unwrap();
    assert!(
        composer
            .clone()
            .load_active(directory.file("b.crod"))
            .is_err()
    );
    let layer = CrolLayer::from_sources("late", [("a.crod", "")]);
    let error = composer.load_layer(layer).compose_data().unwrap_err();
    assert!(error.to_string().contains("is the last layer"), "{error}");
}

#[test]
fn the_active_layer_is_a_crod_file() {
    let directory = Directory::new("kinds");
    let is_error = |file: PathBuf| composer().load_active(file).is_err();
    assert!(is_error(directory.0.clone()));
    assert!(is_error(directory.file("user.crob")));
    assert!(is_error(directory.file("user.crol")));
    let result = composer()
        .without_file_system()
        .load_active(directory.file("a.crod"));
    assert!(result.is_err());
}

#[test]
fn the_active_layer_holds_only_what_a_database_writes() {
    let directory = Directory::new("foreign");
    let file = directory.file("user.crod");
    for text in [
        "+new: 1\n",
        "?new: 1\n",
        "a := 1\n",
        "#![type(Config)]\n",
        "<prod> {\n    <cache> {\n        retries: 1\n    }\n}\n",
        "retries<prod>: 1\n",
        "name = retries: 1\n",
        "-?retries\n",
    ] {
        std::fs::write(&file, text).unwrap();
        let error = composer().load_active(&file).err();
        let error = error.unwrap_or_else(|| panic!("{text}")).to_string();
        assert!(error.contains("which a database does not write"), "{error}");
    }
    std::fs::write(&file, "retries: \n").unwrap();
    let error = composer().load_active(&file).err().unwrap().to_string();
    assert!(error.contains("does not parse"), "{error}");
}

#[test]
fn a_write_needs_a_target() {
    let mut db = db();
    let root = Config::path();
    let value: Config = db.select().get(root.clone()).unwrap();
    let error = db.transaction().at(root).set(value).unwrap_err();
    assert!(error.to_string().contains("needs a path"), "{error}");
}

#[test]
fn with_variant_comes_first() {
    let mut db = db();
    let tx = db
        .transaction()
        .at(Config::path().retries())
        .set(1)
        .unwrap();
    assert!(tx.with_variant("prod").is_err());
}

#[derive(Debug, Serialize, Deserialize, Schema, Navigable)]
#[crol(constraints(positive = Min(0)))]
struct Guarded {
    #[crol(constraint = "positive")]
    hp: i32,
    #[crol(sealed)]
    origin: String,
}

#[test]
fn a_commit_runs_constraints_and_sealed_fields() {
    let base = CrolLayer::from_sources("base", [("a.crod", "+hp: 1\n+origin: \"here\"\n")]);
    let mut db: CrolDb<Guarded> = CrolComposer::new()
        .schema(crol::schema::build::<Guarded>())
        .load_layer(base)
        .compose_db()
        .unwrap();
    let hp = Guarded::path().hp();
    let tx = db.transaction().at(&hp).set(-1).unwrap();
    assert!(tx.commit().is_err());
    let origin = Guarded::path().origin();
    let tx = db
        .transaction()
        .at(&origin)
        .set("there".to_owned())
        .unwrap();
    assert!(tx.commit().is_err());
    assert_eq!(db.select().get(&hp).unwrap(), 1);
    assert_eq!(db.select().get(&origin).unwrap(), "here");
    db.transaction().at(&hp).set(2).unwrap().commit().unwrap();
}

#[test]
fn a_path_through_some_writes_the_field() {
    let mut db = db();
    let value = Config::path().tree().child().some().value();
    db.transaction()
        .at(&value)
        .set(5)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&value).unwrap(), 5);
}

#[test]
fn writes_reach_every_kind_of_step() {
    let mut db = db();
    let name = Config::path().items().at(1).name();
    let text = Config::path().pair().t1();
    let b = Config::path().named().key("b");
    let child = Config::path().tree().child().some().value();
    db.transaction()
        .at(&name)
        .set("zwei".to_owned())
        .unwrap()
        .at(&text)
        .set("y".to_owned())
        .unwrap()
        .at(&b)
        .set_expr(croxp! { & + 1 })
        .unwrap()
        .at(&child)
        .set_expr(croxp! { & + 1 })
        .unwrap()
        .commit()
        .unwrap();
    let select = db.select();
    assert_eq!(select.get(&name).unwrap(), "zwei");
    assert_eq!(select.get(&text).unwrap(), "y");
    assert_eq!(select.get(&b).unwrap(), 3);
    assert_eq!(select.get(&child).unwrap(), 3);
}

#[test]
fn a_path_through_an_option_held_as_a_plain_value() {
    let text = BASE.replace(
        "child: Some({ value: 2, child: None })",
        "child: { value: 2, child: None }",
    );
    let mut db: Db = CrolComposer::new()
        .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
        .compose_db()
        .unwrap();
    let value = Config::path().tree().child().some().value();
    db.transaction()
        .at(&value)
        .set(5)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&value).unwrap(), 5);
}

#[test]
fn a_path_through_an_option_that_is_none() {
    let directory = Directory::new("through-none");
    let file = directory.file("user.crod");
    let mut db = open(&file);
    let deep = Config::path().tree().child().some();
    let value = deep.child().some().value();
    db.transaction()
        .at(&value)
        .set(5)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(db.select().get(&value).unwrap(), 5);
    db.save().unwrap();
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written, "^tree.child?.child?.value: 5\n");
    assert_eq!(open(&file).select().get(&value).unwrap(), 5);
}

#[test]
fn writing_what_an_option_holds_replaces_it() {
    let mut db = db();
    let child = Config::path().tree().child();
    let tree = Tree {
        value: 8,
        child: None,
    };
    let tx = db.transaction().at(child.clone().some()).set(tree.clone());
    tx.unwrap().commit().unwrap();
    assert_eq!(db.select().get(child.some()).unwrap(), tree);
}

#[test]
fn a_string_key_that_is_no_identifier_is_written_as_an_accessor() {
    let mut map = BTreeMap::new();
    map.insert(
        "odd key".to_owned(),
        BTreeMap::from([("a b".to_owned(), 1)]),
    );
    let base = CrolLayer::from_sources("base", [("a.crod", "+\"odd key\": { \"a b\": 1 }\n")]);
    let mut db: CrolDb<BTreeMap<String, BTreeMap<String, i64>>> =
        CrolComposer::new().load_layer(base).compose_db().unwrap();
    let path = BTreeMap::<String, BTreeMap<String, i64>>::path()
        .key("odd key")
        .key("a b");
    db.transaction().at(&path).set(2).unwrap().commit().unwrap();
    assert_eq!(db.select().get(&path).unwrap(), 2);
}

mod views {
    use crol::{CrolComposer, CrolDb, CrolLayer, Navigable as _, schema};
    use crol_derive::{Navigable, Schema, View};
    use serde_derive::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable)]
    struct Combat {
        range: u32,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable, Schema)]
    #[crol(defaults)]
    struct Unit {
        hp: u32,
        #[crol(schema = any)]
        combat: Combat,
        tags: Vec<String>,
        #[serde(default = "three")]
        speed: u32,
    }

    fn three() -> u32 {
        3
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Navigable, Schema)]
    enum Definition {
        Unit(Unit),
    }

    #[derive(Debug, Serialize, Deserialize, Navigable)]
    struct Game {
        artillery: Unit,
        trooper: Unit,
    }

    /// A view of a `Unit`.
    #[derive(Debug, PartialEq, View)]
    struct Stats {
        hp: u32,
        #[crol(path = "combat.range")]
        range: u32,
        armour: Option<u32>,
    }

    const TEXT: &str = r#"
        +artillery: Unit { hp: 1100, combat: { range: 12 }, tags: ["mech"], speed: 1 }
        +trooper: Unit { hp: 50, combat: { range: 5 }, tags: [], speed: 2 }
        artillery<empire>.hp: 1200
    "#;

    fn db() -> CrolDb<Game> {
        let layer = CrolLayer::from_sources("base", [("a.crod", TEXT)]);
        CrolComposer::new().load_layer(layer).compose_db().unwrap()
    }

    fn stats(hp: u32, range: u32) -> Stats {
        Stats {
            hp,
            range,
            armour: None,
        }
    }

    #[test]
    fn select_reads_a_view_at_a_path_in_its_context() {
        let db = db();
        let artillery = Game::path().artillery();
        let base = db.select().view::<Stats>(&artillery).unwrap();
        assert_eq!(base, stats(1100, 12));
        let empire = db.select().with_variant("empire").unwrap();
        assert_eq!(empire.view::<Stats>(&artillery).unwrap(), stats(1200, 12));
    }

    #[test]
    fn a_view_reads_below_a_root_key() {
        #[derive(Debug, PartialEq, View)]
        struct Reach {
            range: u32,
        }
        let db = db();
        let path = Game::path().trooper().combat();
        let reach = db.select().view::<Reach>(path).unwrap();
        assert_eq!(reach, Reach { range: 5 });
    }

    #[test]
    fn a_view_needs_its_path_to_be_set_and_to_reach_a_root_key() {
        let db: CrolDb<Game> = CrolComposer::new()
            .load_layer(CrolLayer::from_sources("base", [("a.crod", "+a: 1")]))
            .compose_db()
            .unwrap();
        let error = db.select().view::<Stats>(Game::path().trooper());
        assert_eq!(
            error.unwrap_err().to_string(),
            "`trooper` is not set in this context"
        );
        let error = db.select().view::<Stats>(Game::path());
        assert!(error.unwrap_err().to_string().contains("a root key"));
    }

    #[test]
    fn a_view_takes_its_default_where_a_get_is_an_error() {
        #[derive(Debug, PartialEq, View)]
        struct Pace {
            speed: u32,
        }
        let text = r#"
            +artillery: Unit { hp: 1, combat: { range: 1 }, tags: [], .. }
            +trooper: Unit { hp: 1, combat: { range: 1 }, tags: [], .. }
        "#;
        let db: CrolDb<Game> = CrolComposer::new()
            .schema(schema::build::<Definition>())
            .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
            .compose_db()
            .unwrap();
        let unit = Game::path().trooper();
        assert_eq!(db.select().view::<Pace>(&unit).unwrap(), Pace { speed: 3 });
        assert!(db.select().get(unit.speed()).is_err());
    }

    #[test]
    fn select_queries_its_context() {
        let db = db();
        let empire = db.select().with_variant("empire").unwrap();
        let query = empire.query();
        let strong = query.holding_at(Unit::path().hp(), &1200);
        assert_eq!(strong.keys().unwrap(), ["artillery"]);
        let base = db.select().query();
        let strong = base.holding_at(Unit::path().hp(), &1200);
        assert!(strong.keys().unwrap().is_empty());
    }

    #[test]
    fn a_typed_query_goes_through_lists_and_fields() {
        let db = db();
        let select = db.select();
        let query = select.query();
        let by_tag = query.holding_at(Unit::path().tags(), "mech");
        assert_eq!(by_tag.keys().unwrap(), ["artillery"]);
        let query = select.query();
        let first = query.holding_at(Unit::path().tags().at(0), "mech");
        assert_eq!(first.keys().unwrap(), ["artillery"]);
        let query = select.query();
        let range = query.holding_at(Unit::path().combat().range(), &5);
        assert_eq!(range.keys().unwrap(), ["trooper"]);
    }

    #[test]
    fn a_transaction_sees_its_own_writes_in_views_and_queries() {
        let mut db = db();
        let trooper = Game::path().trooper();
        let mut tx = db
            .transaction()
            .at(trooper.clone().tags())
            .set(vec!["mech".to_owned()])
            .unwrap()
            .at(trooper.clone().hp())
            .set(60)
            .unwrap();
        assert_eq!(tx.view::<Stats>(&trooper).unwrap(), stats(60, 5));
        let query = tx.query().unwrap();
        let mechs = query.of_type("Unit").holding("tags", "mech");
        assert_eq!(mechs.keys().unwrap(), ["artillery", "trooper"]);
        drop(tx);
        assert_eq!(db.select().view::<Stats>(&trooper).unwrap(), stats(50, 5));
    }

    #[test]
    fn a_view_in_a_scope_starts_at_the_scope() {
        let mut db = db();
        db.transaction()
            .within(Game::path().trooper(), |mut unit| {
                let hp: Stats = unit.view(Unit::path()).unwrap();
                assert_eq!(hp, stats(50, 5));
                Ok(unit)
            })
            .unwrap()
            .commit()
            .unwrap();
    }

    #[test]
    fn a_query_in_a_transaction_can_decide_what_to_write() {
        let mut db = db();
        let mut tx = db.transaction();
        let mechs = tx.query().unwrap().holding("tags", "mech").keys().unwrap();
        assert_eq!(mechs, ["artillery"]);
        let tx = tx.at(Game::path().artillery().hp()).set(2000).unwrap();
        tx.commit().unwrap();
        let view = db.select().view::<Stats>(Game::path().artillery());
        assert_eq!(view.unwrap().hp, 2000);
    }

    #[test]
    fn one_struct_can_be_a_view_and_navigable() {
        #[derive(Debug, PartialEq, View, Navigable)]
        struct Both {
            hp: u32,
            #[crol(path = "combat.range")]
            range: u32,
            #[crol(default)]
            #[crol(leaf)]
            speed: u32,
        }
        let path = Both::path().range();
        assert_eq!(
            format!("{:?}", crol::IntoPath::<Both>::into_path(path)),
            ".combat.range"
        );
        let db = db();
        let both = db.select().view::<Both>(Game::path().trooper()).unwrap();
        assert_eq!(
            both,
            Both {
                hp: 50,
                range: 5,
                speed: 2
            }
        );
    }

    #[test]
    fn a_view_names_a_field_as_serde_does() {
        #[derive(Debug, PartialEq, Deserialize, View, Navigable)]
        #[serde(rename_all = "camelCase")]
        struct Renamed {
            hit_points: u32,
            #[serde(rename = "tags")]
            labels: Vec<String>,
        }
        let text = r#"+a: { hitPoints: 3, tags: ["x"] }"#;
        let data: crol::CrolData = CrolComposer::new()
            .load_layer(CrolLayer::from_sources("base", [("a.crod", text)]))
            .compose_data()
            .unwrap();
        let view = &data.variations::<Renamed>("a").unwrap()[0].value;
        let expected = Renamed {
            hit_points: 3,
            labels: vec!["x".to_owned()],
        };
        assert_eq!(*view, expected);
        let path = crol::IntoPath::<Renamed>::into_path(Renamed::path().hit_points());
        assert_eq!(format!("{path:?}"), ".hitPoints");
    }

    #[test]
    fn a_query_of_a_context_that_was_not_evaluated_fails_when_read() {
        let db: CrolDb<Game> = CrolComposer::with_cfg(crol::CrolConfig {
            variants: crol::CrolVariants::new().add_grouped(["empire", "rebel"]),
            contexts: Some(Vec::new()),
            ..Default::default()
        })
        .load_layer(CrolLayer::from_sources("base", [("a.crod", TEXT)]))
        .compose_db()
        .unwrap();
        let select = db.select().with_variant("empire").unwrap();
        let error = select.query().holding("tags", "mech").keys().unwrap_err();
        assert!(error.to_string().contains("evaluated"), "{error}");
    }
}
