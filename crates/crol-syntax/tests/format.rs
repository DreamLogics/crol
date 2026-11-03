//! Formatter tests. Every case also checks that the output parses without
//! errors and that formatting it again changes nothing.

use crol_syntax::format::{FormatConfig, format};
use expect_test::{Expect, expect};

fn check_with(config: &FormatConfig, text: &str, expect: Expect) {
    let formatted = format(text, config).expect("the input has no syntax errors");
    let parse = crol_syntax::parse(&formatted);
    assert!(
        parse.errors().is_empty(),
        "the output has syntax errors: {:?}\n{formatted}",
        parse.errors()
    );
    let again = format(&formatted, config).unwrap();
    assert_eq!(again, formatted, "formatting is not idempotent");
    expect.assert_eq(&formatted);
}

fn check(text: &str, expect: Expect) {
    check_with(&FormatConfig::default(), text, expect);
}

#[test]
fn spacing() {
    check(
        "+web:Service{replicas:&+1*2,url:\"x\"+&self.port,ids:[@a,@\"b c\"],t:(1,),u:()}",
        expect![[r#"
            +web: Service { replicas: & + 1 * 2, url: "x" + &self.port, ids: [@a, @"b c"], t: (1,), u: () },
        "#]],
    );
}

#[test]
fn binds_spreads_and_references() {
    check(
        "t:={p=port:80,..&base,x:&parent.&parent.y<a&!b>},-web<dev>.limits,c:-&",
        expect![[r#"
            t := { p = port: 80, ..&base, x: &parent.&parent.y<a & !b> },
            -web<dev>.limits,
            c: -&,
        "#]],
    );
}

#[test]
fn long_blocks_break() {
    check_with(
        &FormatConfig::default().with_max_width(40),
        "+web: Service { replicas: 2, image: \"web:1.4\", depends_on: [@database, @cache], .. }",
        expect![[r#"
            +web: Service {
                replicas: 2,
                image: "web:1.4",
                depends_on: [@database, @cache],
                ..,
            },
        "#]],
    );
}

#[test]
fn nested_breaking_is_innermost_last() {
    check_with(
        &FormatConfig::default().with_max_width(30),
        "web: & ~ { env: { LOG_LEVEL: \"debug\", CACHE: \"redis://cache\" }, n: Rgb(1, 2, 3) }",
        expect![[r#"
            web: & ~ {
                env: {
                    LOG_LEVEL: "debug",
                    CACHE: "redis://cache",
                },
                n: Rgb(1, 2, 3),
            },
        "#]],
    );
}

#[test]
fn width_counts_the_trailing_comma() {
    // 19 characters with the comma: fits at 19, breaks at 18.
    check_with(
        &FormatConfig::default().with_max_width(19),
        "a: [1, 2, 3, 4, 5],",
        expect![[r#"
            a: [1, 2, 3, 4, 5],
        "#]],
    );
    check_with(
        &FormatConfig::default().with_max_width(18),
        "a: [1, 2, 3, 4, 5],",
        expect![[r#"
            a: [
                1,
                2,
                3,
                4,
                5,
            ],
        "#]],
    );
}

#[test]
fn defaults_move_to_the_end() {
    check(
        "Service { .., a: 1, .., b: 2 }",
        expect![[r#"
            Service { a: 1, b: 2, .. }
        "#]],
    );
}

#[test]
fn outer_braces_are_removed() {
    check(
        "{ +a: 1, b: 2 }",
        expect![[r#"
            +a: 1,
            b: 2,
        "#]],
    );
    // A typed block is a value, and keeps its braces.
    check(
        "Config { a: 1 }",
        expect![[r#"
            Config { a: 1 }
        "#]],
    );
    // `-12` on its own would be an expression.
    check(
        "{ -12, a: 1 }",
        expect![[r#"
            { -12, a: 1 }
        "#]],
    );
}

#[test]
fn string_keys_are_written_bare() {
    check(
        r#""port": 1, "cache url": 2, "true": 3, x["a"]["b c"][0]: 4, ["k"]: 5, y: &root["web"],"#,
        expect![[r#"
            port: 1,
            "cache url": 2,
            "true": 3,
            x.a["b c"][0]: 4,
            k: 5,
            y: &root.web,
        "#]],
    );
}

#[test]
fn comments_and_blank_lines() {
    check(
        "\n\n// leading\n+a: 1, // trailing\n\n\n\n/* own line */\nb: { // first\n  c: 1,\n\n  // last\n},\n\n",
        expect![[r#"
            // leading
            +a: 1, // trailing

            /* own line */
            b: {
                // first
                c: 1,

                // last
            },
        "#]],
    );
}

#[test]
fn comments_force_a_break() {
    check(
        "a: [1, /* one */ 2],",
        expect![[r#"
            a: [
                1, /* one */
                2,
            ],
        "#]],
    );
}

#[test]
fn entries_with_interior_comments_are_kept_as_written() {
    check(
        "a: 1 + // why\n    2, b:{c:1}",
        expect![[r#"
            a: 1 + // why
                2,
            b: { c: 1 },
        "#]],
    );
}

#[test]
fn variant_blocks() {
    check(
        "<prod&!cache>{web.replicas:&*4},<a+(b|c)>{x:1}",
        expect![[r#"
            <prod & !cache> { web.replicas: & * 4 },
            <a + (b | c)> { x: 1 },
        "#]],
    );
}

#[test]
fn attributes() {
    check(
        "#![use(a::B,c::D<u32,List(String)>as E)]\n#![schema( \"crate:app\" )]\n\nx:1",
        expect![[r##"
            #![use(a::B, c::D<u32, List(String)> as E)]
            #![schema("crate:app")]

            x: 1,
        "##]],
    );
}

#[test]
fn empty_documents() {
    check("", expect![[""]]);
    check(
        "  // only a comment\n",
        expect![[r#"
        // only a comment
    "#]],
    );
    check("{}", expect![[""]]);
}

#[test]
fn syntax_errors_are_returned() {
    let errors = format("a: 1 b: 2", &FormatConfig::default()).unwrap_err();
    assert_eq!(errors.len(), 1);
}

#[test]
fn a_comment_moves_with_its_defaults_marker() {
    check(
        "{ .., // the rest\n a: 1, .., // dropped\n b: 2 }",
        expect![[r#"
            a: 1,
            // dropped
            b: 2,
            .., // the rest
        "#]],
    );
}

#[test]
fn broken_blocks_stay_broken() {
    // A line break after `{` keeps a block broken, even when it would fit;
    // the inner block, written on one line, stays on one line.
    check(
        "<prod> {\n    web.replicas: 4, web.env: { A: 1 } },\n+db: Service {\n port: 1, .. },",
        expect![[r#"
            <prod> {
                web.replicas: 4,
                web.env: { A: 1 },
            },
            +db: Service {
                port: 1,
                ..,
            },
        "#]],
    );
}

#[test]
fn lists_reflow_to_the_width() {
    check(
        "a: [\n    1,\n    2,\n], b: (\n 1, 2),",
        expect![[r#"
            a: [1, 2],
            b: (1, 2),
        "#]],
    );
}
