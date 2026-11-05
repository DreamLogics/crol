//! Evaluating value-mode documents. Each case prints the value as CROL, or
//! the diagnostics as `line:col: message`.

use crol_eval::{Sources, evaluate_value, print};
use expect_test::{Expect, expect};

fn check(text: &str, expect: Expect) {
    let mut sources = Sources::new();
    let file = sources.add("test.crol", text);
    let evaluation = evaluate_value(&sources, file);
    let mut out = String::new();
    if let Some(value) = &evaluation.value {
        out.push_str(&print::to_document(value, &Default::default()));
    }
    for diagnostic in &evaluation.diagnostics {
        let location = sources.location(diagnostic.span);
        out.push_str(&format!(
            "{}:{}: {}",
            location.line_col.line, location.line_col.col, diagnostic.message
        ));
        if let Some(context) = &diagnostic.context {
            out.push_str(&format!(" (while evaluating {context})"));
        }
        out.push('\n');
        for related in &diagnostic.related {
            let location = sources.location(related.span);
            out.push_str(&format!(
                "  {}:{}: {}\n",
                location.line_col.line, location.line_col.col, related.message
            ));
        }
    }
    expect.assert_eq(&out);
}

#[test]
fn literals() {
    check(
        r##"a: 0x1F, b: -170141183460469231731687303715884105728, c: 2.5e-3, d: 'é', e: "x\ty", f: r#"raw "q""#, g: true, h: (), i: (1,), j: @web, k: @"odd id""##,
        expect![[r#"
            a: 31
            b: -170141183460469231731687303715884105728
            c: 0.0025
            d: 'é'
            e: "x\ty"
            f: "raw \"q\""
            g: true
            h: ()
            i: (1,)
            j: @web
            k: @"odd id"
        "#]],
    );
}

#[test]
fn arithmetic_and_concatenation() {
    check(
        r#"a: 7 / 2, b: -7 % 3, c: 1 + 0.5, d: "port " + 8080, e: 'x' + "y" + true, f: [1, 2] + [3], g: [1, 2, 1, 3] - [1], h: 1.0 + 2, i: -(3)"#,
        expect![[r#"
            a: 3
            b: -1
            c: 1.5
            d: "port 8080"
            e: "xytrue"
            f: [1, 2, 3]
            g: [2, 3]
            h: 3.0
            i: -3
        "#]],
    );
}

#[test]
fn arithmetic_errors() {
    check(
        r#"a: [1] + "x", b: 1 / 0, c: 170141183460469231731687303715884105727 + 1, d: 1e300 * 1e300, e: -"s", f: [1] + 2"#,
        expect![[r#"
            1:4: +: cannot add a string to a list (while evaluating a)
            1:18: /: division by zero (while evaluating b)
            1:28: +: integer overflow (while evaluating c)
            1:76: *: the result is not a finite float (while evaluating d)
            1:94: -: cannot negate a string (while evaluating e)
            1:103: +: cannot add an integer to a list (while evaluating f)
        "#]],
    );
}

#[test]
fn self_reads_the_final_value() {
    check(
        r#"#![type(Config)]
port := 8080,
listen: "0.0.0.0:" + &port,
admin: "127.0.0.1:" + (&port + 1),
web: { port: 80, url: "http://x:" + &self.port, nested: { up: &parent.port } },"#,
        expect![[r#"
            Config {
                listen: "0.0.0.0:8080",
                admin: "127.0.0.1:8081",
                web: { port: 80, url: "http://x:80", nested: { up: 80 } },
            }
        "#]],
    );
}

#[test]
fn templates_rebind_through_spreads() {
    // SPEC.md 6.1: `web.url` is computed from web's own port, while the
    // template's `url` stays as it was.
    check(
        r#"service_defaults := {
    port: 80,
    url: "http://localhost:" + &self.port,
},
web: Service { ..&service_defaults, port: 8080, .. },
template_url: &service_defaults.url,"#,
        expect![[r#"
            web: Service { port: 8080, url: "http://localhost:8080", .. }
            template_url: "http://localhost:80"
        "#]],
    );
}

#[test]
fn binds_follow_spreads() {
    // SPEC.md 6.1.1.
    check(
        r#"service_defaults := {
    port = port: 80,
    health: { probe: { url: "http://localhost:" + &port } },
},
web: Service { ..&service_defaults, port: 8080, .. },"#,
        expect![[r#"
            web: Service {
                port: 8080,
                health: { probe: { url: "http://localhost:8080" } },
                ..,
            }
        "#]],
    );
}

#[test]
fn block_steps_and_value_below() {
    // Spreads first, then entries in order; `&` is the block's current value.
    check(
        r#"base := { a: 1, b: 2, c: 3 },
x: { a: 10, ..&base, b: & + 100, +d: 4, ?c: 99, -a },
y: { n: 1, n: & * 5, m: [&self.n] },"#,
        expect![[r#"
            x: { b: 102, c: 3, d: 4, a: (), -a }
            y: { n: 5, m: [5] }
        "#]],
    );
}

#[test]
fn merge_and_paths() {
    check(
        r#"limits := Limits { cpu: 1.0, memory_mb: 512 },
a: &limits ~ { cpu: 2.0 },
b: { l: &limits, l.memory_mb: & * 2, t: (1, 2), t.1: 3, xs: [1, 2, 3], xs[1]: 20, -xs[0] },
c: Some(1) ~ Some(2),
d: (&limits ~ { cpu: 4.0 }).cpu,
e: [10, 20][1],"#,
        expect![[r#"
            a: Limits { cpu: 2.0, memory_mb: 512 }
            b: { l: Limits { cpu: 1.0, memory_mb: 1024 }, t: (1, 3), xs: [20, 3] }
            c: Some(2)
            d: 4.0
            e: 20
        "#]],
    );
}

#[test]
fn merge_errors() {
    check(
        r#"a: Service { x: 1 } ~ Limits { y: 2 }, b: [1] ~ [2], c: Some(1) ~ None"#,
        expect![[r#"
            1:23: ~: cannot merge `Service` with `Limits` (while evaluating a)
            1:43: ~: cannot merge a list with a list; use `:` to replace a value, `+` for lists, or a path (while evaluating b)
            1:57: ~: cannot merge a named tuple with a unit name; use `:` to replace a value, `+` for lists, or a path (while evaluating c)
        "#]],
    );
}

#[test]
fn fallback_catches_missing_values_only() {
    check(
        r#"a: & + 1 | 1,
b: &root.nope | "default",
c: &root.a.x | 3,
d: (1 / 0) | 2,
e: { inner: &root.nope } | 5,"#,
        expect![[r#"
            3:11: .x: cannot select in an integer (while evaluating c)
            4:5: /: division by zero (while evaluating d)
            5:18: .nope: there is no such value (while evaluating e.inner)
        "#]],
    );
}

#[test]
fn entry_errors() {
    check(
        r#"a: { x: 1, +x: 2 }, b: { -y }, c: { y.z: 1 }, d: { t: (1, 2), -t.0 }, e: { l: [1], +l[0]: 2 }, f: &"#,
        expect![[r#"
            1:13: +: `x` already exists (while evaluating a)
              1:9: its value
            1:27: -: `y` does not exist (while evaluating b)
            1:37: y.z: `y` does not exist, so it cannot be patched (while evaluating c)
            1:65: -: tuple elements cannot be removed (while evaluating d.t)
            1:86: +: a list element can only be set, with `:` (while evaluating e.l)
            1:99: &: there is no value below here (while evaluating f)
        "#]],
    );
}

#[test]
fn paths_are_the_whole_text() {
    for path in ["web", "web.port", "web.tags[0]", "web[\"a b\"]", "x.crod"] {
        assert!(crol_eval::parse_path(path).is_ok(), "{path}");
    }
    for text in [
        "", "+web", "-web", "?web", "a: 1, b", "a b", "::a", "./a", "a/b", "a=b", " a",
    ] {
        assert!(crol_eval::parse_path(text).is_err(), "{text}");
    }
}

#[test]
fn cycles() {
    check(
        r#"a: { ..&self.b, b: {} },
c: { d: &self.d },
t := { x: &t },
u: &t,"#,
        expect![[r#"
            1:8: cycle: this value depends on itself (while evaluating a)
              1:1: which is evaluated here
            2:14: cycle: this value depends on itself (while evaluating c.d)
              2:4: which is evaluated here
            3:6: cycle: this value contains itself (while evaluating t.x)
        "#]],
    );
}

#[test]
fn reading_errors() {
    check(
        r#"#![extends(super::x)]
a: &nope,
b: { p = x: 1, p = y: 2 },
self = c: 1,
d: &root.x<a | b>,
e: &<a>,"#,
        expect![[r#"
            1:1: `extends` names layers below a document, and a value-mode document has no layers
            2:4: no bind `nope` is visible; did you mean `&self.nope`?
            3:16: `p` is already bound
              3:6: the other bind
            4:1: `self` cannot be a bind name
            5:4: a variant selector in a reference may only use names, `!` and `&`, since it must name one context
            6:4: a variant selector cannot follow `&` or `&default`, which do not depend on the context
        "#]],
    );
}

#[test]
fn self_without_a_location() {
    check(
        r#"a: 1 + { x: &self.y, y: 1 }.x, b: &parent"#,
        expect![[r#"
        1:13: &self: this block has no location, so it cannot refer to itself or its surroundings (while evaluating a)
        1:35: &parent: there is no block above the top level (while evaluating b)
    "#]],
    );
}

#[test]
fn defaults_flag_and_defaulted_fields() {
    check(
        r#"a: S { x: 1, .. }, b: { ..&root.a, -x }, c: { ..&root.b, x: 2 }"#,
        expect![[r#"
            a: S { x: 1, .. }
            b: { x: (), -x, .. }
            c: { x: 2, .. }
        "#]],
    );
}

#[test]
fn use_aliases() {
    check(
        "#![use(app::db::Config as DbConfig)]\nDbConfig { url: \"postgres://db\" }",
        expect![[r#"
            Config { url: "postgres://db" }
        "#]],
    );
}
