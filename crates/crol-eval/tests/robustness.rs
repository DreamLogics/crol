//! The evaluator never panics or loops on any parsed input (`SPEC.md`
//! 14.2), and printing a value gives text that evaluates back to it.

use crol_eval::{Layer, Sources, evaluate_value, print};

struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.next().is_multiple_of(one_in)
    }

    fn key(&mut self) -> &'static str {
        self.pick(&["a", "b", "c", "port", "x"])
    }

    fn expr(&mut self, depth: u32) -> String {
        if depth > 3 {
            return self.atom();
        }
        match self.next() % 12 {
            0..=2 => self.atom(),
            3 => format!(
                "{} {} {}",
                self.expr(depth + 1),
                self.pick(&["+", "-", "*", "/", "%", "~", "|"]),
                self.expr(depth + 1)
            ),
            4 => format!("[{}, {}]", self.expr(depth + 1), self.expr(depth + 1)),
            5 => self.block(depth + 1),
            6 => {
                format!("&{}", self.pick(&["self", "root", "parent", "t", ""])) + &self.selectors()
            }
            7 => format!("({}, {})", self.expr(depth + 1), self.expr(depth + 1)),
            8 => format!("Some({})", self.expr(depth + 1)),
            9 => format!("-{}", self.expr(depth + 1)),
            10 => format!("({}){}", self.expr(depth + 1), self.selectors()),
            _ => format!("& ~ {}", self.block(depth + 1)),
        }
    }

    fn selectors(&mut self) -> String {
        let mut out = String::new();
        for _ in 0..self.next() % 3 {
            out.push_str(&match self.next() % 3 {
                0 => format!(".{}", self.key()),
                1 => ".0".to_owned(),
                _ => format!("[{}]", self.next() % 3),
            });
        }
        out
    }

    fn atom(&mut self) -> String {
        self.pick(&[
            "1",
            "-2",
            "0.5",
            "\"s\"",
            "'c'",
            "true",
            "()",
            "None",
            "@web",
            "&",
            "170141183460469231731687303715884105727",
        ])
        .to_owned()
    }

    fn block(&mut self, depth: u32) -> String {
        let name = if self.chance(3) { "S " } else { "" };
        let mut entries = Vec::new();
        for _ in 0..self.next() % 4 {
            entries.push(self.entry(depth));
        }
        format!("{name}{{ {} }}", entries.join(", "))
    }

    fn entry(&mut self, depth: u32) -> String {
        let key = self.key();
        match self.next() % 9 {
            0 => format!("+{key}: {}", self.expr(depth)),
            1 => format!("?{key}: {}", self.expr(depth)),
            2 => format!("-{key}"),
            3 => format!(
                "..{}",
                self.pick(&["&t", "&self", "&root", "{ a: 1 }", "&"])
            ),
            4 => "..".to_owned(),
            5 => format!("{key}.{}: {}", self.key(), self.expr(depth)),
            6 => format!("{}{key} = {key}: {}", "q", self.expr(depth)),
            _ => format!("{key}: {}", self.expr(depth)),
        }
    }

    fn document(&mut self) -> String {
        let mut entries = vec![format!("t := {}", self.block(1))];
        for _ in 0..1 + self.next() % 5 {
            entries.push(self.entry(0));
        }
        entries.join(",\n")
    }
}

#[test]
fn random_documents_evaluate_without_panicking() {
    let mut generator = Gen(0xC201_5EED);
    let mut evaluated = 0;
    for _ in 0..3_000 {
        let text = generator.document();
        if !crol_syntax::parse(&text).errors().is_empty() {
            continue;
        }
        let mut sources = Sources::new();
        let file = sources.add("random.crod", text.as_str());
        // `CROL_TRACE=<file>` writes each document there before evaluating
        // it, so that the one that hangs or crashes can be found.
        if std::env::var_os("CROL_TRACE").is_some() {
            std::fs::write(std::env::var("CROL_TRACE").unwrap(), &text).unwrap();
        }
        let evaluation = evaluate_value(&sources, file);
        let Some(value) = evaluation.value else {
            continue;
        };
        evaluated += 1;

        // The printed value evaluates back to an identical value.
        let printed = print::to_document(&value, &Default::default());
        let mut sources = Sources::new();
        let file = sources.add("printed.crod", printed.as_str());
        let again = evaluate_value(&sources, file);
        let Some(again) = again.value else {
            panic!(
                "printed value does not evaluate:\n{text}\n--\n{printed}\n{:?}",
                again.diagnostics
            );
        };
        assert!(
            value.identical(&again),
            "print round trip differs:\n{text}\n--\n{printed}"
        );
    }
    assert!(
        evaluated > 300,
        "only {evaluated} documents evaluated without errors"
    );
}

impl Gen {
    fn condition(&mut self) -> String {
        match self.next() % 5 {
            0 => "a".to_owned(),
            1 => "!b".to_owned(),
            2 => "a & b".to_owned(),
            3 => "a | b".to_owned(),
            _ => "a + b".to_owned(),
        }
    }

    fn simple(&mut self) -> String {
        self.pick(&[
            "1",
            "2",
            "\"s\"",
            "[1, 2]",
            "{ x: 1 }",
            "&root.c | 0",
            "Some(1)",
        ])
        .to_owned()
    }

    fn data_entry(&mut self) -> String {
        let key = self.key();
        match self.next() % 9 {
            0 => format!("<{}> {{ {key}: {} }}", self.condition(), self.simple()),
            1 | 2 => format!("{key}<{}>: {}", self.condition(), self.simple()),
            3 => format!(
                "{key}: &root.{}<{}> | 0",
                self.key(),
                self.pick(&["a", "!a", "b & !a"])
            ),
            4 => format!("{key}: {{ x<{}>: 1, x: 2 }}", self.condition()),
            5 => format!("{key}: & + 1 | 1"),
            _ => format!("{key}: {}", self.simple()),
        }
    }

    fn data_document(&mut self) -> String {
        let mut entries = vec![format!("t := {}", self.simple())];
        for _ in 0..1 + self.next() % 6 {
            entries.push(self.data_entry());
        }
        entries.join(",\n")
    }
}

/// Whether two roots have the same keys with identical values. The order of
/// keys that exist only in some contexts is not kept by printing, which
/// writes them after the base's keys.
fn same_keys(a: &crol_eval::Value, b: &crol_eval::Value) -> bool {
    use crol_eval::ValueKind;
    let (ValueKind::Keyed(a), ValueKind::Keyed(b)) = (&a.kind, &b.kind) else {
        return false;
    };
    a.entries.len() == b.entries.len()
        && a.entries
            .iter()
            .all(|(k, v)| b.entries.get(k).is_some_and(|w| v.identical(w)))
}

#[test]
fn random_layers_with_variants_round_trip_through_all_contexts() {
    use crol_eval::{DataOptions, Layer, evaluate_data};

    let mut generator = Gen(0x7A21_A9E5);
    let mut evaluated = 0;
    let rounds = std::env::var("CROL_ROUNDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(2_000);
    for _ in 0..rounds {
        let base = generator.data_document();
        let local = generator.data_document().replace("t :=", "u :=");
        if [&base, &local]
            .iter()
            .any(|t| !crol_syntax::parse(t).errors().is_empty())
        {
            continue;
        }
        let mut sources = Sources::new();
        let layers = vec![
            Layer {
                name: "base".into(),
                documents: vec![("a.crod".into(), sources.add("base.crod", base.as_str()))],
            },
            Layer {
                name: "local".into(),
                documents: vec![("a.crod".into(), sources.add("local.crod", local.as_str()))],
            },
        ];
        let evaluation = evaluate_data(&sources, &layers, &DataOptions::default());
        let Some(value) = &evaluation.value else {
            continue;
        };
        evaluated += 1;

        let printed = print::to_all_contexts(
            value,
            &evaluation.contexts,
            &evaluation.variants,
            &Default::default(),
        );
        let mut sources = Sources::new();
        let layers = vec![Layer {
            name: "printed".into(),
            documents: vec![(
                "a.crod".into(),
                sources.add("printed.crod", printed.as_str()),
            )],
        }];
        let options = DataOptions::default().with_variants(evaluation.variants.clone());
        let again = evaluate_data(&sources, &layers, &options);
        let context = || format!("{base}\n--\n{local}\n-- printed:\n{printed}");
        let Some(again_value) = &again.value else {
            panic!(
                "the printed data does not evaluate: {:?}\n{}",
                again.diagnostics,
                context()
            );
        };
        assert!(
            again_value.identical(value),
            "the base differs:\n{}",
            context()
        );
        assert_eq!(again.contexts.len(), evaluation.contexts.len());
        for (first, second) in evaluation.contexts.iter().zip(&again.contexts) {
            assert_eq!(first.context, second.context);
            let (v1, v2) = (first.root(value), second.root(again_value));
            assert!(same_keys(&v1, &v2), "a context differs:\n{}", context());
        }
    }
    assert!(
        evaluated > 500,
        "only {evaluated} data sets evaluated without errors"
    );
}

impl Gen {
    /// A value that reads other root keys, some of them only once it has been
    /// spread elsewhere, which what a key reads does not show.
    fn linked(&mut self, keys: usize) -> String {
        let other = self.next() % keys as u64;
        match self.next() % 8 {
            0 => format!("&root.k{other}.a | 0"),
            1 => format!("{{ ..(&root.k{other}.b | {{}}), a: 1 }}"),
            2 => format!("{{ ..(&root.k{other} | {{}}), c: 2 }}"),
            3 => "{ a: 1, b: { x: &parent.k1 | 3, y: &parent.a | 4 } }".to_owned(),
            4 => format!("{{ a: {other}, b: {{ ..&t, y: &parent.a }} }}"),
            5 => format!("{{ b: {{ x: &root.k{other}.b.x | 5 }} }}"),
            6 => format!("& ~ {{ a: {other} }}"),
            _ => format!("{other}"),
        }
    }

    fn linked_document(&mut self) -> String {
        let keys = 2 + self.next() as usize % 6;
        let mut entries = vec!["t := { x: &parent.k0 | 6, z: 1 }".to_owned()];
        for key in 0..keys {
            entries.push(format!("k{key}: {}", self.linked(keys)));
        }
        entries.join(",\n")
    }
}

/// What a data set evaluates to, as its base and its messages in order.
fn evaluated(documents: &[&str]) -> (Option<crol_eval::Value>, Vec<String>) {
    use crol_eval::{DataOptions, evaluate_data};

    let mut sources = Sources::new();
    let layers: Vec<Layer> = documents
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let name = format!("{i}.crod");
            Layer {
                name: name.clone(),
                documents: vec![(name.clone(), sources.add(name, *text))],
            }
        })
        .collect();
    let evaluation = evaluate_data(&sources, &layers, &DataOptions::default());
    let messages = evaluation
        .diagnostics
        .iter()
        .map(|d| format!("{:?} {}", d.span, d.message))
        .collect();
    (evaluation.value, messages)
}

/// Reading the whole root keeps every key's cells, as this key does without
/// being evaluated.
const KEEP_EVERY_KEY: &str = "zz: 1 | &root";

/// The base without the key of [`KEEP_EVERY_KEY`].
fn without_keeper(value: crol_eval::Value) -> crol_eval::Value {
    let mut value = value;
    if let crol_eval::ValueKind::Keyed(keyed) = &mut value.kind {
        keyed
            .entries
            .shift_remove(&crol_eval::Key::Str("zz".into()));
    }
    value
}

#[test]
fn a_freed_key_that_is_read_is_computed_again() {
    // `b.x` is `&parent.y` in `a`, where it reads `a.y`; spread into `b`, it
    // reads the root's `y`, which nothing shows to read.
    let text = "y: { deep: 2 },\na: { inner: { x: &parent.y }, y: 1 },\nb: { ..&root.a.inner },";
    let (value, messages) = evaluated(&[text]);
    assert_eq!(messages, Vec::<String>::new());
    let (kept, _) = evaluated(&[text, KEEP_EVERY_KEY]);
    let (value, kept) = (value.unwrap(), without_keeper(kept.unwrap()));
    assert!(value.identical(&kept), "{value:?}\n{kept:?}");
}

#[test]
fn freeing_keys_changes_neither_values_nor_messages() {
    let mut generator = Gen(0x0E71_C7ED);
    let mut evaluated_well = 0;
    for _ in 0..3_000 {
        let text = generator.linked_document();
        assert!(crol_syntax::parse(&text).errors().is_empty(), "{text}");
        let (value, messages) = evaluated(&[&text]);
        let (kept, kept_messages) = evaluated(&[&text, KEEP_EVERY_KEY]);
        assert_eq!(messages, kept_messages, "{text}");
        match (value, kept) {
            (Some(value), Some(kept)) => {
                let kept = without_keeper(kept);
                assert!(value.identical(&kept), "{text}");
                evaluated_well += 1;
            }
            (None, None) => {}
            _ => panic!("only one of them evaluates:\n{text}"),
        }
    }
    assert!(
        evaluated_well > 300,
        "only {evaluated_well} data sets evaluated without errors"
    );
}

/// What a data set evaluates to with `threads`: the base, every context's
/// root, and every message, each with its place.
fn on_threads(
    documents: &[&str],
    threads: usize,
) -> (
    Option<crol_eval::Value>,
    Vec<(crol_eval::Context, crol_eval::Value)>,
    Vec<String>,
) {
    use crol_eval::{DataOptions, evaluate_data};

    let mut sources = Sources::new();
    let layers: Vec<Layer> = documents
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let name = format!("{i}.crod");
            Layer {
                name: name.clone(),
                documents: vec![(name.clone(), sources.add(name, *text))],
            }
        })
        .collect();
    let options = DataOptions::default().with_threads(Some(threads));
    let evaluation = evaluate_data(&sources, &layers, &options);
    let messages = evaluation
        .diagnostics
        .iter()
        .map(|d| format!("{:?} {}", d.span, d.message))
        .collect();
    let contexts = evaluation.value.as_ref().map_or(Vec::new(), |base| {
        let root = |data: &crol_eval::ContextData| (data.context.clone(), data.root(base));
        evaluation.contexts.iter().map(root).collect()
    });
    (evaluation.value, contexts, messages)
}

#[test]
fn threads_change_neither_values_nor_messages() {
    let mut generator = Gen(0x7EAD_5AFE);
    let mut with_contexts = 0;
    for round in 0..1_500 {
        // Conditions, references to other contexts, and values that spread
        // each other, which fail in some contexts and not in others.
        let base = if round % 2 == 0 {
            generator.data_document()
        } else {
            generator.linked_document()
        };
        let local = generator.data_document().replace("t :=", "u :=");
        if [&base, &local]
            .iter()
            .any(|t| !crol_syntax::parse(t).errors().is_empty())
        {
            continue;
        }
        let documents = [base.as_str(), local.as_str()];
        let (value, contexts, messages) = on_threads(&documents, 1);
        for threads in [2, 3, 8] {
            let (other, other_contexts, other_messages) = on_threads(&documents, threads);
            let text = format!("{threads} threads:\n{base}\n--\n{local}");
            assert_eq!(messages, other_messages, "{text}");
            match (&value, &other) {
                (Some(a), Some(b)) => assert!(a.identical(b), "{text}"),
                (None, None) => {}
                _ => panic!("only one of them evaluates: {text}"),
            }
            assert_eq!(contexts.len(), other_contexts.len(), "{text}");
            for ((context, a), (other, b)) in contexts.iter().zip(&other_contexts) {
                assert_eq!(context, other, "{text}");
                assert!(a.identical(b), "{text}");
            }
        }
        with_contexts += usize::from(contexts.len() > 2);
    }
    assert!(
        with_contexts > 100,
        "only {with_contexts} data sets had enough contexts"
    );
}

#[test]
fn leaving_out_keys_that_cannot_differ_changes_nothing() {
    use crol_eval::{DataOptions, evaluate_data};

    let mut generator = Gen(0xD1FF_E2E7);
    let mut compared = 0;
    for _ in 0..3_000 {
        let base = generator.data_document();
        let local = generator.data_document().replace("t :=", "u :=");
        if [&base, &local]
            .iter()
            .any(|t| !crol_syntax::parse(t).errors().is_empty())
        {
            continue;
        }
        let mut sources = Sources::new();
        let layers = vec![
            Layer {
                name: "base".into(),
                documents: vec![("a.crod".into(), sources.add("base.crod", base.as_str()))],
            },
            Layer {
                name: "local".into(),
                documents: vec![("a.crod".into(), sources.add("local.crod", local.as_str()))],
            },
        ];
        let options = DataOptions::default();
        let mut every = DataOptions::default();
        every.evaluate_every_key = true;
        let shared = evaluate_data(&sources, &layers, &options);
        let all = evaluate_data(&sources, &layers, &every);
        let (Some(value), Some(all_value)) = (&shared.value, &all.value) else {
            continue;
        };
        let text = || format!("{base}\n--\n{local}");
        assert_eq!(shared.contexts.len(), all.contexts.len(), "{}", text());
        for (a, b) in shared.contexts.iter().zip(&all.contexts) {
            assert_eq!(a.context, b.context);
            let (a, b) = (a.root(value), b.root(all_value));
            assert!(same_keys(&a, &b), "{}", text());
        }
        compared += 1;
    }
    assert!(compared > 500, "only {compared} data sets evaluated");
}

/// Inputs the fuzz targets (`fuzz/`) found problems with.
#[test]
fn fuzz_findings() {
    let inputs: &[&str] = &[
        // A char literal that is only `'`, at the end of the text.
        "a: '",
        // Unterminated strings ending in a character of several bytes, or
        // with nothing after the hashes.
        "a: \"Ŝ",
        "a: r#\"Ŝ",
        "a: r##",
    ];
    for input in inputs {
        let mut sources = Sources::new();
        let file = sources.add("a.crod", *input);
        let layers = [crol_eval::Layer {
            name: "base".to_owned(),
            documents: vec![("a.crod".to_owned(), file)],
        }];
        let evaluation =
            crol_eval::evaluate_data(&sources, &layers, &crol_eval::DataOptions::default());
        assert!(evaluation.has_errors(), "{input:?}");
        let _ = evaluate_value(&sources, file);
    }
}

/// Whatever a `.crob` file says, reading and evaluating it never panics or
/// loops: this changes what it holds past the hash, as a crafted file would.
#[test]
fn damaged_compiled_layers_do_not_panic() {
    let text = "t := { a: 1, b: & + 1 }\n+x: Unit { ..&t, c: [1, 2], d: { e: &self.a } }\n\
                x<rebel>.a: round!(2.5)\n+y: &root.x.a + 1\n-z\n..\n";
    let mut sources = Sources::new();
    let file = sources.add("a.crod", text);
    let layer = Layer {
        name: "base".into(),
        documents: vec![("a.crod".into(), file)],
    };
    let bytes = crol_eval::encode_layer(&sources, &layer).unwrap_or_else(|d| panic!("{d:?}"));
    let lowered = crol_eval::binary::read_layer(&bytes).unwrap().lowered;
    let layers = [layer];
    let run = |bytes: &[u8]| {
        let (compiled, _) = crol_eval::compile_binary(&sources, &layers, &[Some(bytes)]);
        if let Some(compiled) = compiled {
            let _ = crol_eval::evaluate_compiled(&compiled, &Default::default());
        }
    };
    run(&lowered);
    for end in 0..lowered.len() {
        run(&lowered[..end]);
    }
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20_000 {
        let mut changed = lowered.to_vec();
        for _ in 0..1 + next() % 4 {
            let at = (next() % changed.len() as u64) as usize;
            changed[at] = next() as u8;
        }
        run(&changed);
    }
}

#[test]
fn a_damaged_compiled_file_is_refused() {
    let mut sources = Sources::new();
    let file = sources.add("a.crod", "+x: 1");
    let layer = Layer {
        name: "base".into(),
        documents: vec![("a.crod".into(), file)],
    };
    let bytes = crol_eval::encode_layer(&sources, &layer).unwrap_or_else(|d| panic!("{d:?}"));
    let mut changed = bytes.clone();
    *changed.last_mut().unwrap() ^= 1;
    assert!(
        crol_eval::binary::read_layer(&changed)
            .unwrap_err()
            .contains("damaged")
    );
    assert!(crol_eval::binary::read_layer(b"nonsense").is_err());
    let mut other = bytes;
    other[4] = 99;
    assert!(
        crol_eval::binary::read_layer(&other)
            .unwrap_err()
            .contains("format 99")
    );
}

/// A file with the right hash can still lie about what it holds.
#[test]
fn a_compiled_file_cannot_ask_for_more_than_it_can_hold() {
    let header = |size: u64, packed: &[u8]| {
        let mut file = b"CROB".to_vec();
        file.extend_from_slice(&crol_eval::binary::FORMAT.to_le_bytes());
        file.extend_from_slice(blake3::hash(packed).as_bytes());
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(packed);
        file
    };
    for size in [u64::MAX, 1 << 40, 100_000] {
        let file = header(size, b"tiny");
        let message = crol_eval::binary::read_layer(&file).unwrap_err();
        assert!(message.contains("damaged"), "{message}");
    }
    let file = header(100, &[0xFF; 64]);
    assert!(crol_eval::binary::read_layer(&file).is_err());
    assert!(crol_eval::binary::read_layer(&header(0, b"")).is_err());
}
