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
        .set_expr(croxp! { &root.y | 3 })
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
