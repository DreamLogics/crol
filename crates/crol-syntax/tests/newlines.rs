//! A line break as a separator of entries (`SPEC.md` 5.3).

use crol_syntax::SyntaxKind;

/// The tree of `text`, without trivia and commas, so that a text with line
/// breaks can be compared to the same text with commas.
fn shape(text: &str) -> Vec<String> {
    let parse = crol_syntax::parse(text);
    assert!(parse.errors().is_empty(), "{text:?}: {:?}", parse.errors());
    assert_eq!(parse.syntax().to_string(), text, "not lossless");
    parse
        .syntax()
        .descendants_with_tokens()
        .filter(|it| !it.kind().is_trivia() && it.kind() != SyntaxKind::COMMA)
        .map(|it| match it.into_token() {
            Some(token) => format!("{:?} {}", token.kind(), token.text()),
            None => "node".to_owned(),
        })
        .collect()
}

#[track_caller]
fn same(with_line_breaks: &str, with_commas: &str) {
    assert_eq!(
        shape(with_line_breaks),
        shape(with_commas),
        "{with_line_breaks:?} reads differently from {with_commas:?}"
    );
}

fn errors(text: &str) -> Vec<String> {
    let parse = crol_syntax::parse(text);
    assert_eq!(parse.syntax().to_string(), text, "not lossless");
    parse
        .errors()
        .iter()
        .map(|e| e.message().to_owned())
        .collect()
}

#[track_caller]
fn fails(text: &str) {
    assert!(!errors(text).is_empty(), "{text:?} parses");
}

#[test]
fn a_line_break_begins_each_kind_of_entry() {
    same("a: 1\nb: 2", "a: 1, b: 2");
    same("a: 1\n+b: 2", "a: 1, +b: 2");
    same("a: 1\n?b: 2", "a: 1, ?b: 2");
    same("a: &\n? b: 2", "a: &, ?b: 2");
    same("a: 1\n-b", "a: 1, -b");
    same("a: 1\n-\"b c\"", "a: 1, -\"b c\"");
    same("a: 1\n-r\"b c\"", "a: 1, -r\"b c\"");
    same("a: 1\n<p> { b: 2 }", "a: 1, <p> { b: 2 }");
    same("a: 1\n..&t", "a: 1, ..&t");
    same("a: 1\nb = c: 2", "a: 1, b = c: 2");
    same("a: 1\nb = +c: 2", "a: 1, b = +c: 2");
    same("a: 1\nb := 2", "a: 1, b := 2");
    same("a: 1\n\"b c\": 2", "a: 1, \"b c\": 2");
    same("a: 1\n12: 2", "a: 1, 12: 2");
    same("a: b\n[0]: 2", "a: b, [0]: 2");
    same("a: b\n[-3].c<p>: 2", "a: b, [-3].c<p>: 2");
    same(
        "a: 1\n+\"b c\".d[0]<p & !q>: 2",
        "a: 1, +\"b c\".d[0]<p & !q>: 2",
    );
}

#[test]
fn every_block_takes_line_breaks() {
    same("{ a: 1\n+b: 2 }", "{ a: 1, +b: 2 }");
    same("Foo {\n a: 1\n b: 2\n}", "Foo { a: 1, b: 2 }");
    same("<p> {\n a: 1\n -b\n}", "<p> { a: 1, -b }");
    same("a: {\n b: 1\n +c: 2\n}\nd: 3", "a: { b: 1, +c: 2 }, d: 3");
    // A block in brackets too.
    same("a: [{ b: 1\n+c: 2 }]", "a: [{ b: 1, +c: 2 }]");
    same("a: ({ b: 1\n-c })", "a: ({ b: 1, -c })");
}

#[test]
fn commas_and_line_breaks_mix() {
    same("a: 1,\nb: 2", "a: 1, b: 2");
    same("a: 1\n, b: 2", "a: 1, b: 2");
    same("a: 1\nb: 2,\n", "a: 1, b: 2");
    fails("a: 1,\n,b: 2");
}

#[test]
fn entries_on_one_line_need_a_comma() {
    assert_eq!(
        errors("a: 1 b: 2"),
        ["expected `,` or a line break between entries"]
    );
    fails("a: 1 /* c */ b: 2");
    fails("{ a: 1 b: 2 }");
    // A line break in a string is not one between tokens.
    fails("a: \"x\ny\" b: 2");
    fails("a: 1\rb: 2");
}

#[test]
fn what_counts_as_a_line_break() {
    same("a: 1\r\nb: 2\r\n+c: 3", "a: 1, b: 2, +c: 3");
    same("a: 1 // one\n+b: 2", "a: 1, +b: 2");
    same("a: 1 // one\n// two\n\n\n+b: 2 // three", "a: 1, +b: 2");
    same("a: 1 /* one\n two */ +b: 2", "a: 1, +b: 2");
    same("a: 1\n/* one */ +b: 2", "a: 1, +b: 2");
    same("a: 1 /* one */\n+b: 2", "a: 1, +b: 2");
}

#[test]
fn values_continue_over_lines() {
    same("a: 1\n+ 2", "a: 1 + 2");
    same("a: 1\n+ b", "a: 1 + b");
    same("a: \"x\"\n  + \"y\"", "a: \"x\" + \"y\"");
    same("a: 1\n- 2", "a: 1 - 2");
    same("a: 1\n-2", "a: 1 - 2");
    same("a: &x\n- &y", "a: &x - &y");
    same("a: [1]\n- [1]", "a: [1] - [1]");
    same("a: &\n-[\"x\"]", "a: & - [\"x\"]");
    same("a: 1\n* 2", "a: 1 * 2");
    same("a: 1\n/ 2", "a: 1 / 2");
    same("a: 1\n% 2", "a: 1 % 2");
    same("a: &\n~ { b: 1 }", "a: & ~ { b: 1 }");
    same("a: &\n?? 1", "a: & ?? 1");
    same("a: &\n| [1]", "a: & | [1]");
    same("a: b\n.c", "a: b.c");
    same("a: b\n[0]", "a: b[0]");
    same("a: b\n[\"c\"].d", "a: b[\"c\"].d");
    same("a: b\n<p>", "a: b<p>");
    same("a: b\n<p>.c", "a: b<p>.c");
    same("a: Foo\n(1)", "a: Foo(1)");
    same("a: Foo\n{ b: 1 }", "a: Foo { b: 1 }");
    same("a:\n1", "a: 1");
    same("a:\n-b", "a: -b");
    same("a: 1 +\n2", "a: 1 + 2");
    same("a: 1 +\nb", "a: 1 + b");
    same("a := 1\n+ 2", "a := 1 + 2");
    same("a\n: 1", "a: 1");
    same("a\n.b\n[0]\n<p>: 1", "a.b[0]<p>: 1");
    same("-a\n.b", "-a.b");
    same("-a\n[0]", "-a[0]");
    same("..\n&x", "..&x");
    same("..\n{ a: 1 }", "..{ a: 1 }");
    same("a: 1\n + 2\nb: 3\n", "a: 1 + 2, b: 3");
}

#[test]
fn a_value_stops_before_a_line_that_begins_an_entry() {
    same("-a\n[0]: 1", "-a, [0]: 1");
    same("-a\n<p> { b: 1 }", "-a, <p> { b: 1 }");
    same("..&x\n[0]: 1", "..&x, [0]: 1");
    same("..\nb: 1", ".., b: 1");
    same("..\n+b: 1", ".., +b: 1");
    same("..\n-b", ".., -b");
    same("..\n<p> { b: 1 }", ".., <p> { b: 1 }");
    same("..\n..&x", "..,  ..&x");
    same("..\nb = c: 1", ".., b = c: 1");
    same("a: &\nb: 1", "a: &, b: 1");
    same("a: &\nb.c: 1", "a: &, b.c: 1");
    same("a: &\nb := 1", "a: &, b := 1");
    same("a: &\n+b: 1", "a: &, +b: 1");
    same("a: &\n-b", "a: &, -b");
}

#[test]
fn in_brackets_a_line_break_means_nothing() {
    same("a: (1\n-b)", "a: (1 - b)");
    same("a: [1\n-b]", "a: [1 - b]");
    same("a: Foo(1\n-b)", "a: Foo(1 - b)");
    same("a: [b\n[0], 2]", "a: [b[0], 2]");
    same("a: (1,\n2\n)", "a: (1, 2)");
    assert_eq!(errors("a: [1\n2]"), ["expected `,`"]);
    fails("a: (1\n+b: 2)");
    fails("a: [1\n+b: 2]");
    fails("a: Foo(1\n2)");
}

#[test]
fn an_expression_document_has_no_entries_to_separate() {
    same("1\n+ 2", "1 + 2");
    same("&x\n-b", "&x - b");
    same("Foo { a: 1 }\n.a", "Foo { a: 1 }.a");
    assert_eq!(errors("1\n2"), ["expected the end of the document"]);
    assert_eq!(errors("[1]\n+b: 2"), ["expected the end of the document"]);
    fails("{ a: 1 }\n{ b: 2 }");
}

#[test]
fn a_line_with_no_entry_shape_fails_loudly() {
    // The target and `:` are not on the line of the `+`.
    fails("a: 1\n+b\n.c: 2");
    fails("a: 1\n+\nb: 2");
    fails("a: 1\n+b\n: 2");
    // An operator at the end of a line takes the next one in.
    fails("a: 1 +\nb: 2");
    fails("a: 1 -\nb: 2");
    fails("a: 1 +\n+b: 2");
    // Neither an entry nor a continuation.
    fails("a: 1\n@b");
    fails("a: 1\n(2)");
    fails("a: 1\n{ b: 2 }");
    fails("a: &x\n<p\n{ b: 1 }");
    // The `{` of a variant block on the next line.
    fails("a: b\n<p>\n{ c: 1 }");
    // `-b` is a whole entry, so what follows it cannot be.
    fails("a: 5\n-b * 2");
    fails("a: 5\n-Foo(1)");
    fails("a: &x\n-b ?? 5");
}

#[test]
fn a_remove_of_an_integer_or_accessor_key_needs_its_comma() {
    same("a: 5,\n-12", "a: 5, -12");
    same("a: [1],\n-[0]", "a: [1], -[0]");
    let message =
        "expected `,`: a line break does not separate entries before `-` and an integer or `[`";
    assert_eq!(errors("-a\n-12"), [message]);
    assert_eq!(errors("<p> { b: 1 }\n-[0]"), [message]);
}

#[test]
fn an_error_stays_on_its_line() {
    // One error each; the entries after them are still there.
    assert_eq!(errors("a: 1 @b @c\nd: 2\ne: 3").len(), 1);
    assert_eq!(errors("a: 1\n@b\nd: 2").len(), 1);
    assert_eq!(errors("x: 1\na\nb: 2\nc: 3"), ["expected `:`"]);
    assert_eq!(errors("a:\nb: 2\nc: 3"), ["expected a value after `:`"]);
    assert_eq!(errors("a: 1\nb\nc: 3"), ["expected `:`"]);
    assert_eq!(errors("+a\n+b: 2"), ["expected `:`"]);
    let parse = crol_syntax::parse("a: 1 @b\nc: 2\n+d: 3");
    let text = parse.syntax().to_string();
    assert_eq!(text, "a: 1 @b\nc: 2\n+d: 3");
    let entries = parse
        .syntax()
        .children()
        .filter(|n| n.kind() == SyntaxKind::ENTRY)
        .count();
    assert_eq!(entries, 3);
}
