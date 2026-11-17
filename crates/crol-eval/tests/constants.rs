//! Constants the program gives (`SPEC.md` 6.1.4): `$name`, in both modes.

use crol_eval::{
    Constants, DataOptions, Evaluation, Key, Keyed, Layer, Sources, Span, Value, ValueKind,
    evaluate_data, evaluate_value_with, print,
};
use expect_test::{Expect, expect};

fn int(i: i128) -> Value {
    Value::new(ValueKind::Int(i), Span::detached())
}

fn string(s: &str) -> Value {
    Value::new(ValueKind::String(s.into()), Span::detached())
}

fn limits() -> Value {
    let mut keyed = Keyed {
        name: Some("Limits".into()),
        ..Keyed::default()
    };
    keyed.entries.insert(Key::Str("cpu".into()), int(2));
    keyed.entries.insert(Key::Str("memory_mb".into()), int(512));
    keyed.defaults = true;
    Value::new(ValueKind::Keyed(Box::new(keyed)), Span::detached())
}

fn constants() -> Constants {
    let mut constants = Constants::new();
    constants.insert("foo_count", int(42));
    constants.insert("home", string("/home/me"));
    constants.insert("limits", limits());
    constants.insert(
        "ports",
        Value::new(ValueKind::List(vec![int(80), int(443)]), Span::detached()),
    );
    constants.insert(
        "pair",
        Value::new(ValueKind::Tuple(vec![int(1), string("b")]), Span::detached()),
    );
    constants.insert("root", int(7));
    constants
}

fn render(sources: &Sources, evaluation: &Evaluation) -> String {
    let mut out = String::new();
    if let Some(value) = &evaluation.value {
        out.push_str(&print::to_document(value, &Default::default()));
    }
    for diagnostic in &evaluation.diagnostics {
        let location = sources.location(diagnostic.span);
        out.push_str(&format!(
            "{:?} {}:{}: {}\n",
            diagnostic.severity, location.line_col.line, location.line_col.col, diagnostic.message
        ));
    }
    out
}

fn check_value(text: &str, constants: &Constants, expect: Expect) {
    let mut sources = Sources::new();
    let file = sources.add("test.crol", text);
    let evaluation = evaluate_value_with(&sources, file, constants);
    expect.assert_eq(&render(&sources, &evaluation));
}

fn check_data(layers: &[&str], options: &DataOptions, expect: Expect) {
    let mut sources = Sources::new();
    let layers: Vec<Layer> = layers
        .iter()
        .enumerate()
        .map(|(i, text)| Layer {
            name: format!("l{i}"),
            documents: vec![("a.crod".into(), sources.add(format!("l{i}/a.crod"), *text))],
        })
        .collect();
    let evaluation = evaluate_data(&sources, &layers, options);
    expect.assert_eq(&render(&sources, &evaluation));
}

#[test]
fn value_mode_reads_constants_and_selectors() {
    check_value(
        r#"
        count: $foo_count + 1,
        dir: $home + "/data",
        cpu: $limits.cpu,
        limits: $limits ~ { cpu: 4 },
        https: $ports[1],
        second: $pair.1,
        root: $root,
        "#,
        &constants(),
        expect![[r#"
            count: 43
            dir: "/home/me/data"
            cpu: 2
            limits: Limits { cpu: 4, memory_mb: 512 }
            https: 443
            second: "b"
            root: 7
        "#]],
    );
}

#[test]
fn a_constant_not_given_is_missing() {
    check_value(
        r#"
        replicas: $replicas ?? 1,
        cpu: $limits.gpu ?? 0,
        count: $foo_cuont,
        "#,
        &constants(),
        expect![[r#"
            Error 4:16: no constant `foo_cuont` was given; did you mean `$foo_count`?
        "#]],
    );
}

#[test]
fn a_failed_constant_is_not_caught() {
    let mut constants = constants();
    constants.fail("declared");
    check_value(
        "a: $declared ?? 1, b: 2",
        &constants,
        expect![""],
    );
}

#[test]
fn tools_warn_of_a_constant_without_a_value() {
    let constants = Constants::new().with_absent_warning(true);
    check_value(
        "a: $x + 1, b: 2",
        &constants,
        expect![[r#"
            Warning 1:4: no constant `x` was given
        "#]],
    );
}

#[test]
fn a_variant_selector_on_a_constant_is_an_error() {
    check_value(
        "a: $foo_count<prod>",
        &constants(),
        expect![[r#"
            Error 1:4: a variant selector cannot follow `$foo_count`: a constant is the same in every context
        "#]],
    );
}

#[test]
fn data_mode_definitions_and_patches() {
    let options = DataOptions::default().with_constants(constants());
    check_data(
        &[
            "field: $foo_count, other: 1, <prod> { +p: $home }",
            "field: & + $foo_count, other: $foo_count",
        ],
        &options,
        expect![[r#"
            field: 84
            other: 42
        "#]],
    );
}

#[test]
fn templates_read_constants() {
    let options = DataOptions::default().with_constants(constants());
    check_data(
        &["t := { n: $foo_count }, +a: { ..&t, m: &self.n + 1 }"],
        &options,
        expect![[r#"
            a: { n: 42, m: 43 }
        "#]],
    );
}
