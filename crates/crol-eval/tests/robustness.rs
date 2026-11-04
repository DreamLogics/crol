//! The evaluator never panics or loops on any parsed input (`SPEC.md`
//! 14.2), and printing a value gives text that evaluates back to it.

use crol_eval::{Sources, evaluate_value, print};

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
        let file = sources.add("random.crol", text.as_str());
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
        let file = sources.add("printed.crol", printed.as_str());
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
                documents: vec![("a.crol".into(), sources.add("base.crol", base.as_str()))],
            },
            Layer {
                name: "local".into(),
                documents: vec![("a.crol".into(), sources.add("local.crol", local.as_str()))],
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
                "a.crol".into(),
                sources.add("printed.crol", printed.as_str()),
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
        let file = sources.add("a.crol", *input);
        let layers = [crol_eval::Layer {
            name: "base".to_owned(),
            documents: vec![("a.crol".to_owned(), file)],
        }];
        let evaluation =
            crol_eval::evaluate_data(&sources, &layers, &crol_eval::DataOptions::default());
        assert!(evaluation.has_errors(), "{input:?}");
        let _ = evaluate_value(&sources, file);
    }
}
