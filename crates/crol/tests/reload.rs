//! `CrolData::reload` evaluates again only the keys a change can reach
//! (`SPEC.md` 7.9). These tests make random changes to small data and check
//! that a reload gives what loading everything afresh gives.

use std::collections::BTreeMap;

use crol::{CrolComposer, CrolConfig, CrolData, CrolLayer, CrolVariants, DocumentChange};

/// A small, fixed pseudo-random generator, so that a failure can be repeated.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

const KEYS: usize = 8;

/// The documents, by layer and path.
type Texts = BTreeMap<(&'static str, String), String>;

/// A value for the key `k{i}`, which may read keys before it.
fn expr(rng: &mut Rng, i: usize) -> String {
    let n = rng.below(10);
    if i == 0 {
        return n.to_string();
    }
    let j = rng.below(i);
    match rng.below(4) {
        0 => format!("&root.k{j}.v + {n}"),
        1 => format!("&root.k{j}.v | {n}"),
        _ => n.to_string(),
    }
}

/// The definitions of the keys `k{i}` with `i % 2 == half`, with some
/// conditional patches of their own.
fn definitions(rng: &mut Rng, half: usize) -> String {
    let mut text = String::new();
    for i in (half..KEYS).step_by(2) {
        if rng.chance(85) {
            let w = rng.below(5);
            text += &format!("+k{i}: {{ v: {}, w: {w} }},\n", expr(rng, i));
            if rng.chance(30) {
                text += &format!("k{i}<empire>.v: {},\n", expr(rng, i));
            }
        }
    }
    text
}

/// Patches to random keys, from a document that comes after the definitions.
fn patches(rng: &mut Rng, templates: bool) -> String {
    let mut text = String::new();
    for _ in 0..rng.below(4) {
        let i = rng.below(KEYS);
        text += &match rng.below(6) {
            0 => format!("k{i}.w: & * 2,\n"),
            1 => format!("k{i}<rebels>: {{ v: {}, w: 0 }},\n", expr(rng, i)),
            2 => format!("k{i}<hard>.w: & + 1,\n"),
            3 if templates => format!("k{i}.w: &t.w,\n"),
            4 => format!("n{i}: {{ v: &root.k{i}.v | 0 }},\n"),
            _ => format!("k{i}.v: {},\n", expr(rng, i)),
        };
    }
    if rng.chance(10) {
        text += &format!("-k{},\n", rng.below(KEYS));
    }
    text
}

/// A change to one document: a document's text made again, or a document
/// added or taken away.
fn edit(rng: &mut Rng, texts: &Texts) -> ((&'static str, String), Option<String>) {
    match rng.below(12) {
        0..=2 => (("base", "a.crod".to_owned()), Some(definitions(rng, 0))),
        3..=5 => (("base", "b.crod".to_owned()), Some(definitions(rng, 1))),
        6 | 7 => (("base", "sub/c.crod".to_owned()), Some(patches(rng, false))),
        8 | 9 => (("mod", "x.crod".to_owned()), Some(patches(rng, true))),
        10 => {
            let w = rng.below(5);
            (
                ("base", "sub/t.crod".to_owned()),
                Some(format!("t := {{ w: {w} }},\n")),
            )
        }
        _ => {
            let key = ("mod", "more/y.crod".to_owned());
            let text = (!texts.contains_key(&key)).then(|| patches(rng, true));
            (key, text)
        }
    }
}

fn compose(texts: &Texts) -> Result<CrolData, crol::Error> {
    let layer = |name: &str| {
        let documents = texts
            .iter()
            .filter(|((layer, _), _)| *layer == name)
            .map(|((_, path), text)| (path.clone(), text.clone()));
        CrolLayer::from_sources(name.to_owned(), documents)
    };
    let variants = CrolVariants::new()
        .add_grouped(["empire", "rebels"])
        .add_discrete("hard");
    CrolComposer::with_cfg(CrolConfig {
        variants,
        ..Default::default()
    })
    .load_layer(layer("base"))
    .load_layer(layer("mod"))
    .compose_data()
}

/// Whether two data sets are the same: every key's value in every context,
/// in the same order.
fn assert_same(data: &CrolData, fresh: &CrolData, texts: &Texts) {
    let say = || format!("after these documents: {texts:#?}");
    assert!(
        data.root_value().identical(&fresh.root_value()),
        "{}",
        say()
    );
    assert_eq!(data.fingerprint(), fresh.fingerprint(), "{}", say());
    assert_eq!(
        data.key_fingerprints(),
        fresh.key_fingerprints(),
        "{}",
        say()
    );
    // What each context changes, removes and defaults, in order.
    let (view, fresh_view) = (data.view(), fresh.view());
    assert_eq!(view.contexts.len(), fresh_view.contexts.len(), "{}", say());
    for (a, b) in view.contexts.iter().zip(fresh_view.contexts) {
        assert_eq!(a.context, b.context, "{}", say());
        assert_eq!(
            (a.defaults, &a.defaulted),
            (b.defaults, &b.defaulted),
            "{}",
            say()
        );
        let keys = |data: &crol_eval::ContextData| data.changes.keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(a), keys(b), "{}", say());
        for (x, y) in a.changes.values().zip(b.changes.values()) {
            let same = match (x, y) {
                (Some(x), Some(y)) => x.identical(y),
                (x, y) => x.is_none() && y.is_none(),
            };
            assert!(same, "{x:?} and {y:?}; {}", say());
        }
    }
    let contexts: Vec<_> = data.contexts().collect();
    assert_eq!(contexts, fresh.contexts().collect::<Vec<_>>(), "{}", say());
    for context in &contexts {
        let keys = data.keys_in(context);
        assert_eq!(keys, fresh.keys_in(context), "{}", say());
        for key in &keys {
            let (a, b) = (data.value(key, context), fresh.value(key, context));
            assert!(
                a.zip(b).is_some_and(|(a, b)| a.identical(b)),
                "`{key}` in {:?}: {a:?} and {b:?}; {}",
                data.context_names(context),
                say()
            );
        }
    }
}

fn walk(seed: u64, steps: usize) -> (usize, usize) {
    let mut rng = Rng(seed);
    let mut texts = Texts::new();
    texts.insert(
        ("base", "sub/t.crod".to_owned()),
        "t := { w: 1 },\n".to_owned(),
    );
    let mut data = loop {
        for (key, text) in [
            (("base", "a.crod".to_owned()), definitions(&mut rng, 0)),
            (("base", "b.crod".to_owned()), definitions(&mut rng, 1)),
            (("base", "sub/c.crod".to_owned()), patches(&mut rng, false)),
            (("mod", "x.crod".to_owned()), patches(&mut rng, true)),
        ] {
            texts.insert(key, text);
        }
        if let Ok(data) = compose(&texts) {
            break data;
        }
    };
    let (mut quick, mut reloads) = (0, 0);
    for _ in 0..steps {
        let ((layer, path), text) = edit(&mut rng, &texts);
        let mut next = texts.clone();
        match &text {
            Some(text) => next.insert((layer, path.clone()), text.clone()),
            None => next.remove(&(layer, path.clone())),
        };
        let before = data.key_fingerprints();
        let change = DocumentChange {
            layer,
            path: &path,
            text: text.as_deref(),
        };
        match (compose(&next), data.reload(&[change])) {
            (Ok(fresh), Ok(reloaded)) => {
                assert_same(&data, &fresh, &next);
                let changed = crol::changed_keys(&before, &fresh.key_fingerprints());
                assert_eq!(
                    reloaded.changed, changed,
                    "after these documents: {next:#?}"
                );
                reloads += 1;
                quick += usize::from(reloaded.evaluated.is_some());
                texts = next;
            }
            (Err(_), Err(_)) => {
                // The data stays as it was.
                assert_same(&data, &compose(&texts).unwrap(), &texts);
            }
            (fresh, reloaded) => panic!(
                "loading afresh gives {:?}, and reloading {:?}, after these documents: {next:#?}",
                fresh.map(|_| ()),
                reloaded.map(|_| ())
            ),
        }
    }
    (quick, reloads)
}

#[test]
fn a_reload_gives_what_loading_afresh_gives() {
    let (mut quick, mut reloads) = (0, 0);
    for seed in 1..=12u64 {
        let (q, r) = walk(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), 150);
        quick += q;
        reloads += r;
    }
    assert!(reloads > 600, "only {reloads} reloads succeeded");
    // Most changes reach a few keys; a template that changes, or a
    // document that comes or goes, is evaluated in full.
    assert!(
        quick * 2 > reloads,
        "{quick} of {reloads} reloads were quick"
    );
}

#[test]
fn a_reload_evaluates_only_the_keys_a_change_reaches() {
    let texts = Texts::from([
        (
            ("base", "a.crod".to_owned()),
            "+a: { v: 1 },\n+b: { v: &root.a.v + 1 },\n".to_owned(),
        ),
        (
            ("base", "c.crod".to_owned()),
            "+c: { v: 3 },\n+d: { v: 4 },\n".to_owned(),
        ),
        (("mod", "x.crod".to_owned()), "c<empire>.v: 5,\n".to_owned()),
    ]);
    let mut data = compose(&texts).unwrap();
    let reloaded = data
        .reload(&[DocumentChange {
            layer: "base",
            path: "a.crod",
            text: Some("+a: { v: 2 },\n+b: { v: &root.a.v + 1 },\n"),
        }])
        .unwrap();
    // `a` changed, and `b` reads it; `c` and `d` were not evaluated again.
    assert_eq!(reloaded.evaluated, Some(2));
    assert_eq!(
        reloaded.changed,
        [
            crol::KeyChange::Changed("a".to_owned()),
            crol::KeyChange::Changed("b".to_owned())
        ]
    );
    #[derive(serde_derive::Deserialize)]
    struct Unit {
        v: u32,
    }
    let empire = data.context(["empire"]).unwrap();
    assert_eq!(data.get_in::<Unit>("b", &empire).unwrap().v, 3);
    assert_eq!(data.get_in::<Unit>("c", &empire).unwrap().v, 5);
}
