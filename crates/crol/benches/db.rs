//! What reads and transactions cost on evaluated data: queries, views, and
//! what a read after a write pays for. It writes its own data, so that it
//! needs no files. Run with `cargo bench -p crol --bench db`.

#![expect(clippy::print_stdout, reason = "a benchmark prints its timings")]
#![allow(missing_docs)]

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::Instant;

use crol::{CrolComposer, CrolDb, CrolLayer, DocumentChange, Error, Navigable as _};
use crol_derive::{Navigable, View};
use serde_derive::{Deserialize, Serialize};

const KEYS: usize = 10_000;

#[derive(Serialize, Deserialize, Navigable)]
struct Combat {
    range: u32,
    reload: u32,
}

#[derive(Serialize, Deserialize, Navigable)]
struct Unit {
    hp: u32,
    combat: Combat,
    tags: Vec<String>,
}

/// Two of the keys: the rest are reached by queries.
#[derive(Serialize, Deserialize, Navigable)]
struct Game {
    u0: Unit,
    u1: Unit,
}

#[derive(View)]
struct Stats {
    hp: u32,
    #[crol(path = "combat.range")]
    range: u32,
    armour: Option<u32>,
}

impl Stats {
    fn sum(&self) -> u32 {
        self.hp + self.range + self.armour.unwrap_or(0)
    }
}

fn text() -> String {
    let mut text = String::new();
    for i in 0..KEYS {
        let tag = if i % 10 == 0 { "mech" } else { "foot" };
        let range = i % 50;
        let _ = writeln!(
            text,
            "+u{i}: Unit {{ hp: {i}, combat: {{ range: {range}, reload: 2 }}, \
             tags: [\"a\", \"b\", \"{tag}\"] }}",
        );
    }
    text
}

/// The best of five rounds, so that a busy machine counts for little.
fn time<T>(
    name: &str,
    repeats: u32,
    mut run: impl FnMut() -> Result<T, Error>,
) -> Result<(), Error> {
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..repeats {
            black_box(run()?);
        }
        best = best.min(start.elapsed().as_secs_f64() / f64::from(repeats));
    }
    println!("{name:<36} {:>12.1} us", best * 1e6);
    Ok(())
}

fn main() -> Result<(), Error> {
    let text = text();
    let layer = CrolLayer::from_sources("base", [("a.crod", text.as_str())]);
    let mut db: CrolDb<Game> = CrolComposer::new().load_layer(layer).compose_db()?;
    let unit = Game::path().u1();
    let hp = Game::path().u1().hp();
    let tags = Unit::path().tags();

    println!("{KEYS} keys");
    let data = db.data();
    time("query, no conditions", 50, || data.query().keys())?;
    time("query, a list", 50, || {
        data.query().holding("tags", "mech").keys()
    })?;
    time("query, a nested field", 50, || {
        data.query().holding("combat.range", &7).keys()
    })?;
    time("query, views of a tenth", 50, || {
        let views = data.query().holding("tags", "mech").views::<Stats>()?;
        Ok(views.iter().map(|(_, view)| view.sum()).sum::<u32>())
    })?;
    time("select get, a whole value", 5000, || {
        db.select().get(&unit).map(|unit| unit.hp)
    })?;
    time("select view", 5000, || {
        db.select().view::<Stats>(&unit).map(|view| view.sum())
    })?;

    println!("what a read after a write pays for");
    time("clone the data", 5, || Ok(db.data().clone()))?;
    let mut reloaded = db.data().clone();
    let mut reload = |text: &'static str| {
        let change = DocumentChange {
            layer: "(db)",
            path: "db.crod",
            text: Some(text),
        };
        reloaded.reload(&[change]).map(|_| ())
    };
    time("reload an empty document", 5, || reload(""))?;
    time("reload a one-line document", 5, || reload("^u1.hp: 1\n"))?;

    println!("the same over a compiled layer, which is not parsed again");
    let layer = CrolLayer::from_sources("base", [("a.crod", text.as_str())]);
    let binary = CrolLayer::from_binary("base", &layer.to_binary()?)?;
    let mut compiled: CrolDb<Game> = CrolComposer::new().load_layer(binary).compose_db()?;
    time("write, view", 5, || {
        let mut tx = compiled.transaction().at(&hp).set(1)?;
        tx.view::<Stats>(&unit).map(|view| view.sum())
    })?;

    println!("transactions");
    time("write, dropped", 5000, || {
        db.transaction().at(&hp).set(1).map(drop)
    })?;
    time("view, nothing pending", 5000, || {
        let mut tx = db.transaction();
        tx.view::<Stats>(&unit).map(|view| view.sum())
    })?;
    time("write, view", 5, || {
        let mut tx = db.transaction().at(&hp).set(1)?;
        tx.view::<Stats>(&unit).map(|view| view.sum())
    })?;
    time("write, query", 5, || {
        let mut tx = db.transaction().at(&hp).set(1)?;
        let query = tx.query()?.holding_at(&tags, "mech");
        query.keys().map(|keys| keys.len())
    })?;
    time("two writes, view", 5, || {
        let tx = db.transaction().at(&hp).set(1)?;
        let mut tx = tx.at(&hp).set(2)?;
        tx.view::<Stats>(&unit).map(|view| view.sum())
    })?;
    time("write, view, write, view", 5, || {
        let mut tx = db.transaction().at(&hp).set(1)?;
        tx.view::<Stats>(&unit)?;
        let mut tx = tx.at(&hp).set(2)?;
        tx.view::<Stats>(&unit).map(|view| view.sum())
    })?;
    let mut value = 0;
    time("write, commit", 5, || {
        value += 1;
        db.transaction().at(&hp).set(value)?.commit().map(drop)
    })?;
    Ok(())
}
