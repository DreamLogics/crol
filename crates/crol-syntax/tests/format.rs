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
            +web: Service { replicas: & + 1 * 2, url: "x" + &self.port, ids: [@a, @"b c"], t: (1,), u: () }
        "#]],
    );
}

#[test]
fn binds_spreads_and_references() {
    check(
        "t:={p=port:80,..&base,x:&parent.&parent.y<a&!b>},-web<dev>.limits,c:-&",
        expect![[r#"
            t := { p = port: 80, ..&base, x: &parent.&parent.y<a & !b> }
            -web<dev>.limits
            c: -&
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
            }
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
            }
        "#]],
    );
}

#[test]
fn width_counts_the_trailing_comma() {
    // 23 characters with the indentation and the comma.
    let text = "B {\n    a: [1, 2, 3, 4, 5],\n}";
    check_with(
        &FormatConfig::default().with_max_width(23),
        text,
        expect![[r#"
            B {
                a: [1, 2, 3, 4, 5],
            }
        "#]],
    );
    check_with(
        &FormatConfig::default().with_max_width(22),
        text,
        expect![[r#"
            B {
                a: [
                    1,
                    2,
                    3,
                    4,
                    5,
                ],
            }
        "#]],
    );
}

#[test]
fn width_has_no_comma_to_count_at_the_top_level() {
    check_with(
        &FormatConfig::default().with_max_width(18),
        "a: [1, 2, 3, 4, 5],",
        expect![[r#"
            a: [1, 2, 3, 4, 5]
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
            +a: 1
            b: 2
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
            port: 1
            "cache url": 2
            "true": 3
            x.a["b c"][0]: 4
            k: 5
            y: &root.web
        "#]],
    );
}

#[test]
fn comments_and_blank_lines() {
    check(
        "\n\n// leading\n+a: 1, // trailing\n\n\n\n/* own line */\nb: { // first\n  c: 1,\n\n  // last\n},\n\n",
        expect![[r#"
            // leading
            +a: 1 // trailing

            /* own line */
            b: {
                // first
                c: 1,

                // last
            }
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
            ]
        "#]],
    );
}

#[test]
fn entries_with_interior_comments_are_kept_as_written() {
    check(
        "a: 1 + // why\n    2, b:{c:1}",
        expect![[r#"
            a: 1 + // why
                2
            b: { c: 1 }
        "#]],
    );
}

#[test]
fn variant_blocks() {
    check(
        "<prod&!cache>{web.replicas:&*4},<a+(b|c)>{x:1}",
        expect![[r#"
            <prod & !cache> { web.replicas: & * 4 }
            <a + (b | c)> { x: 1 }
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

            x: 1
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
            a: 1
            // dropped
            b: 2
            .. // the rest
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
            }
            +db: Service {
                port: 1,
                ..,
            }
        "#]],
    );
}

#[test]
fn lists_reflow_to_the_width() {
    check(
        "a: [\n    1,\n    2,\n], b: (\n 1, 2),",
        expect![[r#"
            a: [1, 2]
            b: (1, 2)
        "#]],
    );
}

#[test]
fn integers_keep_a_space_before_a_dot() {
    // `1.16` and `1.0` would be floats.
    check(
        "1  .16:7,a:1 .0,b:-1  .0,c:x.0",
        expect![[r#"
            1 .16: 7
            a: 1 .0
            b: -1 .0
            c: x.0
        "#]],
    );
}

#[test]
fn top_level_entries_lose_their_commas_and_nested_ones_keep_them() {
    check(
        "a: 1, +b: { c: 1, d: 2 }, -e, ?f: [1, 2], g := 3, <prod> { h: 1, i: 2 }, ..&t, j: Foo {\n k: 1\n l: 2\n}",
        expect![[r#"
            a: 1
            +b: { c: 1, d: 2 }
            -e
            ?f: [1, 2]
            g := 3
            <prod> { h: 1, i: 2 }
            ..&t
            j: Foo {
                k: 1,
                l: 2,
            }
        "#]],
    );
}

#[test]
fn outer_braces_count_as_the_top_level_unless_they_stay() {
    check(
        "{\n    a: 1,\n    b: 2,\n}",
        expect![[r#"
            a: 1
            b: 2
        "#]],
    );
    check(
        "Config {\n    a: 1\n    b: 2\n}",
        expect![[r#"
            Config {
                a: 1,
                b: 2,
            }
        "#]],
    );
}

#[test]
fn a_comma_stays_where_a_line_break_would_not_separate() {
    check(
        "a: 5, -12, b: [1], -[0], c: 1, -\"odd key\", d: 2, -[\"plain\"]",
        expect![[r#"
            a: 5,
            -12
            b: [1],
            -[0]
            c: 1
            -"odd key"
            d: 2
            -plain
        "#]],
    );
    // A comment or a blank line in between changes nothing.
    check(
        "a: 5, // five\n\n// then\n-12",
        expect![[r#"
            a: 5, // five

            // then
            -12
        "#]],
    );
    // An entry kept as written may not begin on one line.
    check(
        "a: 5, + // plus\n b: 1",
        expect![[r#"
            a: 5,
            + // plus
             b: 1
        "#]],
    );
}

#[test]
fn the_entries_of_a_comma_free_file_stay_apart() {
    check(
        "a: &x\n+b: 1\nc: &y\n-d\ne: &z\n[\"f g\"]: 2\nh: &w\n<prod> { i: 1 }\nj: &\nk: 1 // end\n-l\n[\"m n\"]: 3",
        expect![[r#"
            a: &x
            +b: 1
            c: &y
            -d
            e: &z
            ["f g"]: 2
            h: &w
            <prod> { i: 1 }
            j: &
            k: 1 // end
            -l
            ["m n"]: 3
        "#]],
    );
}

#[test]
fn continued_expressions_join_their_line() {
    check(
        "a: 1\n  + 2\n  - 3\nb: \"x\"\n  + \"y\"\nc: &x\n  .y\n  [0]\nd: Foo\n  (1)\ne: &\n  ~ { f: 1 }",
        expect![[r#"
            a: 1 + 2 - 3
            b: "x" + "y"
            c: &x.y[0]
            d: Foo(1)
            e: & ~ { f: 1 }
        "#]],
    );
}

#[test]
fn wrapping_never_starts_a_line_with_an_operator() {
    let text = "a: [1, 2, 3] + [4, 5, 6] - [7, 8, 9]\nb: { c: 1, d: 2 } ~ { e: 3, f: 4 }\ng: 1";
    check_with(
        &FormatConfig::default().with_max_width(12),
        text,
        expect![[r#"
            a: [
                1,
                2,
                3,
            ] + [
                4,
                5,
                6,
            ] - [
                7,
                8,
                9,
            ]
            b: {
                c: 1,
                d: 2,
            } ~ {
                e: 3,
                f: 4,
            }
            g: 1
        "#]],
    );
}

#[test]
fn crlf_and_block_comments_separate_too() {
    check(
        "a: 1\r\n+b: 2\r\nc: 3 /* x\ny */ d: 4",
        expect![[r#"
            a: 1
            +b: 2
            c: 3 /* x
            y */
            d: 4
        "#]],
    );
}
