/**
 * @file A tree-sitter grammar for CROL, the Composable Rust Object Language.
 * @license MIT OR Apache-2.0
 *
 * It follows section 9 of SPEC.md and accepts what the Rust parser
 * (crates/crol-syntax) accepts; CI checks both against the same syntax
 * cases. Node names follow the Rust parser's syntax kinds.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const IDENTIFIER = /[A-Za-z][A-Za-z0-9_]*|_[A-Za-z0-9_]+/;
const ESCAPE = /\\(n|r|t|\\|0|"|'|x[0-7][0-9a-fA-F]|u\{[0-9a-fA-F_]{1,6}\}|\r?\n)/;
const STRING = seq('"', repeat(choice(/[^"\\]/, ESCAPE)), '"');

const PREC = {
  fallback: 1,
  merge: 2,
  sum: 3,
  product: 4,
  prefix: 5,
  path: 6,
  cond_or: 1,
  cond_xor: 2,
  cond_and: 3,
  cond_not: 4,
};

/**
 * One or more of `rule`, separated by commas, with an optional trailing one.
 * @param {RuleOrLiteral} rule
 */
function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

/** @param {RuleOrLiteral} rule */
function commaSep(rule) {
  return optional(commaSep1(rule));
}

module.exports = grammar({
  name: 'crol',

  // `_line_break` is a line break that ends an entry (SPEC.md 5.3): empty,
  // and only produced where the next line begins one. See src/scanner.c.
  externals: $ => [$.raw_string, $.block_comment, $._line_break],

  extras: $ => [/\s/, $.line_comment, $.block_comment],

  word: $ => $.identifier,

  conflicts: $ => [
    // At the start of a document, `a` may be a key or a unit name.
    [$.key, $.unit_name],
    [$.key, $.literal],
    [$.key, $.block],
    // At the start of a document, `["x"]` may be an accessor key or a list.
    [$.accessor, $.literal],
  ],

  rules: {
    document: $ => seq(
      repeat($.attribute),
      optional(choice($._entries, $._expression)),
    ),

    // ---- Attributes (SPEC.md 4.1) ----

    attribute: $ => seq(
      '#', token.immediate('!'), '[',
      choice($.use_attr, $.schema_attr, $.type_attr, $.extends_attr, $.variant_attr),
      ']',
    ),

    use_attr: $ => seq('use', '(', commaSep1($.use_path), ')'),
    use_path: $ => seq(
      $._path_segments,
      optional($.type_arg_list),
      optional($.use_alias),
    ),
    use_alias: $ => seq('as', field('name', $.identifier)),
    type_arg_list: $ => seq('<', commaSep1($.type_expr), '>'),
    // SPEC.md 6.1.2: `u32`, `app::Pair<A, B>`, `[T; 3]`, `(A, B)`.
    type_expr: $ => choice(
      seq($._path_segments, optional($.type_arg_list)),
      seq('[', $.type_expr, ';', field('length', $.integer), ']'),
      seq('(', commaSep($.type_expr), ')'),
    ),
    _path_segments: $ => seq($.identifier, repeat(seq('::', $.identifier))),

    schema_attr: $ => seq('schema', '(', choice($.string, $.raw_string, $.module_path), ')'),
    type_attr: $ => seq('type', '(', field('name', $.identifier), ')'),
    extends_attr: $ => seq('extends', '(', $.module_path, ')'),
    variant_attr: $ => seq(
      'variant', '(', field('group', $.identifier), ':', field('name', $.identifier), ')',
    ),

    module_path: $ => seq(
      choice('::', repeat(seq('super', '::'))),
      $._path_segments,
    ),

    // ---- Entries (SPEC.md 5) ----

    _entries: $ => seq(
      $._entry,
      repeat(seq(choice(',', $._line_break), $._entry)),
      optional(','),
    ),

    _entry: $ => choice(
      $.entry,
      $.remove_entry,
      $.value_bind,
      $.variant_block,
      $.spread,
      $.defaults,
    ),

    entry: $ => seq(
      optional($.bind),
      optional(field('op', choice('+', '?', '^'))),
      field('target', $.target),
      ':',
      field('value', $._expression),
    ),
    bind: $ => seq(field('name', $.identifier), '='),
    remove_entry: $ => seq('-', field('target', $.target)),
    value_bind: $ => seq(
      optional(field('attr', $.entry_attr)),
      field('name', $.identifier),
      ':=',
      field('value', $._expression),
    ),

    // `#[type(...)]` (SPEC.md 6.1.2).
    entry_attr: $ => seq('#', '[', 'type', '(', $.type_ref, ')', ']'),
    type_ref: $ => seq($.type_expr, repeat($.type_step)),
    type_step: $ => seq('.', choice(
      field('name', $.identifier),
      field('index', alias(token.immediate(/[0-9]+/), $.tuple_index)),
      seq('[', ']'),
    )),
    variant_block: $ => seq(field('variant', $.variant), $._block_body),
    spread: $ => seq('..', field('operand', $._postfix)),
    defaults: _ => '..',

    // A `?` directly follows a step that is no condition, and directly
    // precedes the next step (SPEC.md 5.1).
    target: $ => seq(field('key', $.key), optional($._after_step)),
    _after_step: $ => choice(
      seq($._optional_step, optional($._after_step)),
      seq($._step, optional($._after_step)),
      seq($.variant, optional($._after_condition)),
    ),
    _after_condition: $ => choice(
      seq($._step, optional($._after_step)),
      seq($.variant, optional($._after_condition)),
    ),
    _step: $ => choice($.field_selector, $.index_selector, $.accessor),
    _optional_step: $ => seq(token.immediate('?'), choice(
      alias($._adjacent_field, $.field_selector),
      alias($._adjacent_index, $.index_selector),
      alias($._adjacent_accessor, $.accessor),
    )),
    _adjacent_field: $ => seq(token.immediate('.'), field('name', $.identifier)),
    _adjacent_index: $ => seq(
      token.immediate('.'),
      field('index', alias(token.immediate(/[0-9]+/), $.tuple_index)),
    ),
    _adjacent_accessor: $ => seq(
      token.immediate('['),
      choice($.string, $.raw_string, seq(optional('-'), $.integer)),
      ']',
    ),
    key: $ => choice($.identifier, $.string, $.raw_string, $.integer, $.accessor),

    _selector: $ => choice(
      $.field_selector,
      $.index_selector,
      $.accessor,
      $.variant,
    ),
    field_selector: $ => seq('.', field('name', $.identifier)),
    index_selector: $ => seq('.', field('index', alias(token.immediate(/[0-9]+/), $.tuple_index))),
    accessor: $ => seq(
      '[',
      choice($.string, $.raw_string, seq(optional('-'), $.integer)),
      ']',
    ),

    // ---- Variants (SPEC.md 8.2) ----

    variant: $ => seq('<', $._condition, repeat(seq('+', $._condition)), '>'),
    _condition: $ => choice($.cond_name, $.cond_not, $.cond_binary, $.cond_paren),
    cond_name: $ => $.identifier,
    cond_not: $ => prec(PREC.cond_not, seq('!', $._condition)),
    cond_paren: $ => seq('(', $._condition, ')'),
    cond_binary: $ => choice(
      prec.left(PREC.cond_or, seq($._condition, '|', $._condition)),
      prec.left(PREC.cond_xor, seq($._condition, '^', $._condition)),
      prec.left(PREC.cond_and, seq($._condition, '&', $._condition)),
    ),

    // ---- Expressions (SPEC.md 6) ----

    _expression: $ => choice($.binary_expr, $.prefix_expr, $._postfix),

    binary_expr: $ => choice(
      prec.left(PREC.fallback, seq(field('lhs', $._expression), field('op', '|'), field('rhs', $._expression))),
      prec.left(PREC.merge, seq(field('lhs', $._expression), field('op', choice('~', '~^')), field('rhs', $._expression))),
      prec.left(PREC.sum, seq(field('lhs', $._expression), field('op', choice('+', '-')), field('rhs', $._expression))),
      prec.left(PREC.product, seq(field('lhs', $._expression), field('op', choice('*', '/', '%')), field('rhs', $._expression))),
    ),
    prefix_expr: $ => prec(PREC.prefix, seq(choice('-', '!'), field('operand', $._expression))),

    _postfix: $ => choice($.path_expr, $._primary),
    path_expr: $ => prec.left(PREC.path, seq(field('base', $._primary), repeat1($._selector))),

    _primary: $ => choice(
      $.literal,
      $.id_expr,
      $.reference,
      $.block,
      $.list_expr,
      $.tuple_expr,
      $.named_tuple,
      $.macro_call,
      $.unit_name,
      $.paren_expr,
    ),

    literal: $ => choice($.string, $.raw_string, $.char, $.integer, $.float, 'true', 'false'),

    // `@name` and `@"text"` are one token each: the name must follow `@`
    // directly (SPEC.md 2.5).
    id_expr: $ => choice(
      alias(token(seq('@', IDENTIFIER)), $.id),
      alias(token(seq('@', STRING)), $.id),
    ),

    // A reference is one token, so that its name follows `&` directly
    // (SPEC.md 2.5). At equal length a string beats a pattern, so `&self`
    // is the keyword and `&selfie` a bind; `&parent.&parent` continues
    // with one token per `.&parent`. In a condition, these tokens are not
    // valid, so `&` there is the operator.
    reference: $ => choice(
      '&',
      alias('&self', $.self),
      alias('&root', $.root),
      alias('&default', $.default),
      seq(alias('&parent', $.parent), repeat(alias(token(seq('.', /\s*/, '&parent')), $.parent))),
      alias(token(seq('&', IDENTIFIER)), $.bind_name),
    ),

    block: $ => seq(optional(field('type', $.identifier)), $._block_body),
    _block_body: $ => seq('{', optional($._entries), '}'),

    list_expr: $ => seq('[', commaSep($._expression), ']'),
    tuple_expr: $ => choice(
      seq('(', ')'),
      seq('(', $._expression, ',', commaSep($._expression), ')'),
    ),
    paren_expr: $ => seq('(', $._expression, ')'),
    named_tuple: $ => seq(field('name', $.identifier), $.arg_list),
    macro_call: $ => seq(field('name', $.identifier), '!', $.arg_list),
    arg_list: $ => seq('(', commaSep($._expression), ')'),
    unit_name: $ => $.identifier,

    // ---- Tokens (SPEC.md 2) ----

    identifier: _ => IDENTIFIER,
    integer: _ => token(choice(
      /0x[0-9a-fA-F_]*[0-9a-fA-F][0-9a-fA-F_]*/,
      /0o[0-7_]*[0-7][0-7_]*/,
      /0b[01_]*[01][01_]*/,
      /[0-9][0-9_]*/,
    )),
    float: _ => token(choice(
      /[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?/,
      /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/,
    )),
    string: _ => token(STRING),
    char: _ => token(seq("'", choice(/[^'\\\n]/, ESCAPE), "'")),
    line_comment: _ => token(seq('//', /[^\n]*/)),
  },
});
