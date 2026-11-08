// The external scanner of the CROL grammar: raw strings (`r#"..."#`, with any
// number of `#`) and block comments, which nest, since neither is a regular
// language, and line breaks that end an entry (SPEC.md 5.3), which depend on
// the shape of the next line.
//
// `LINE_BREAK` is empty. It is only valid after an entry, so the grammar
// itself keeps line breaks in `(...)` and `[...]` meaningless. The scanner
// looks past whitespace and comments at the next line, and produces it when
// that line begins an entry; the parser then ends the value before it.
//
// A block comment directly after the value is the one case where the scanner
// cannot both look past the text and leave it to be lexed: it produces the
// comment, and remembers in `line_break_pending` to produce `LINE_BREAK` on
// the next call, which directly follows.

#include "tree_sitter/parser.h"

#include <stdlib.h>
#include <wctype.h>

enum TokenType {
  RAW_STRING,
  BLOCK_COMMENT,
  LINE_BREAK,
};

typedef struct {
  bool line_break_pending;
} Scanner;

void *tree_sitter_crol_external_scanner_create(void) {
  return calloc(1, sizeof(Scanner));
}

void tree_sitter_crol_external_scanner_destroy(void *payload) { free(payload); }

unsigned tree_sitter_crol_external_scanner_serialize(void *payload, char *buffer) {
  buffer[0] = ((Scanner *)payload)->line_break_pending;
  return 1;
}

void tree_sitter_crol_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  ((Scanner *)payload)->line_break_pending = length > 0 && buffer[0];
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static bool scan_raw_string(TSLexer *lexer) {
  // Past `r`.
  unsigned hashes = 0;
  while (lexer->lookahead == '#') {
    hashes++;
    advance(lexer);
  }
  // `r#type` or a plain identifier: not a raw string.
  if (lexer->lookahead != '"') return false;
  advance(lexer);
  for (;;) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == '"') {
      advance(lexer);
      unsigned closing = 0;
      while (closing < hashes && lexer->lookahead == '#') {
        closing++;
        advance(lexer);
      }
      if (closing == hashes) {
        lexer->mark_end(lexer);
        lexer->result_symbol = RAW_STRING;
        return true;
      }
    } else {
      advance(lexer);
    }
  }
}

// Past `/*`: to the end of a block comment. Sets `line_break` if it holds one.
static bool skip_block_comment(TSLexer *lexer, bool *line_break) {
  unsigned depth = 1;
  while (depth > 0) {
    // An unterminated comment is an error, as in the Rust parser.
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == '\n') *line_break = true;
    if (lexer->lookahead == '/') {
      advance(lexer);
      if (lexer->lookahead == '*') {
        advance(lexer);
        depth++;
      }
    } else if (lexer->lookahead == '*') {
      advance(lexer);
      if (lexer->lookahead == '/') {
        advance(lexer);
        depth--;
      }
    } else {
      advance(lexer);
    }
  }
  return true;
}

// Skips whitespace and comments, looking for a line break. Fails at an
// unterminated comment.
static bool skip_trivia(TSLexer *lexer, bool *line_break) {
  for (;;) {
    if (lexer->lookahead == '\n') *line_break = true;
    if (iswspace(lexer->lookahead)) {
      advance(lexer);
    } else if (lexer->lookahead == '/') {
      advance(lexer);
      if (lexer->lookahead == '/') {
        while (lexer->lookahead != '\n' && !lexer->eof(lexer)) advance(lexer);
      } else if (lexer->lookahead == '*') {
        advance(lexer);
        if (!skip_block_comment(lexer, line_break)) return false;
      } else {
        // `/` itself: an operator, and what the callers see as "other".
        return true;
      }
    } else {
      return true;
    }
  }
}

// The rest of this file reads an entry's shape on one line (SPEC.md 5.3).
// Each function stops at a line break by failing.
static bool skip_inline_trivia(TSLexer *lexer) {
  bool line_break = false;
  return skip_trivia(lexer, &line_break) && !line_break;
}

static bool is_identifier_char(int32_t c) {
  return c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9');
}

static bool is_digit(int32_t c) { return c >= '0' && c <= '9'; }

static void skip_word(TSLexer *lexer) {
  while (is_identifier_char(lexer->lookahead)) advance(lexer);
}

static bool skip_string(TSLexer *lexer) {
  // At `"`.
  advance(lexer);
  for (;;) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == '"') {
      advance(lexer);
      return true;
    }
    if (lexer->lookahead == '\\') advance(lexer);
    advance(lexer);
  }
}

// A key that is a word, or a raw string, which starts like one.
static bool skip_word_or_raw_string(TSLexer *lexer) {
  if (lexer->lookahead != 'r') {
    skip_word(lexer);
    return true;
  }
  advance(lexer);
  if (lexer->lookahead == '"' || lexer->lookahead == '#') {
    unsigned hashes = 0;
    while (lexer->lookahead == '#') {
      hashes++;
      advance(lexer);
    }
    if (lexer->lookahead != '"') return false;
    advance(lexer);
    for (;;) {
      if (lexer->eof(lexer)) return false;
      if (lexer->lookahead == '"') {
        advance(lexer);
        unsigned closing = 0;
        while (closing < hashes && lexer->lookahead == '#') {
          closing++;
          advance(lexer);
        }
        if (closing == hashes) return true;
      } else {
        advance(lexer);
      }
    }
  }
  skip_word(lexer);
  return true;
}

// `[key]`, at `[`.
static bool skip_accessor(TSLexer *lexer) {
  advance(lexer);
  if (!skip_inline_trivia(lexer)) return false;
  if (lexer->lookahead == '-') {
    advance(lexer);
    if (!skip_inline_trivia(lexer)) return false;
  }
  if (lexer->lookahead == '"') {
    if (!skip_string(lexer)) return false;
  } else if (is_identifier_char(lexer->lookahead)) {
    if (!skip_word_or_raw_string(lexer)) return false;
  } else {
    return false;
  }
  if (!skip_inline_trivia(lexer)) return false;
  if (lexer->lookahead != ']') return false;
  advance(lexer);
  return true;
}

// `<condition>`, at `<`.
static bool skip_variant(TSLexer *lexer) {
  advance(lexer);
  for (;;) {
    if (!skip_inline_trivia(lexer)) return false;
    int32_t c = lexer->lookahead;
    if (c == '>') {
      advance(lexer);
      return true;
    }
    if (is_identifier_char(c)) {
      skip_word(lexer);
    } else if (c == '!' || c == '&' || c == '|' || c == '^' || c == '+' || c == '(' || c == ')') {
      advance(lexer);
    } else {
      return false;
    }
  }
}

// After a key: selectors, then `:`.
static bool at_selectors_and_colon(TSLexer *lexer) {
  for (;;) {
    if (!skip_inline_trivia(lexer)) return false;
    switch (lexer->lookahead) {
      case '=':
        // A bind. Anything else before `=` fails later, in the grammar.
        return true;
      case ':':
        // `:` or `:=`, which is a value bind in the same way.
        advance(lexer);
        return lexer->lookahead != ':';
      case '.':
        advance(lexer);
        if (!skip_inline_trivia(lexer)) return false;
        if (!is_identifier_char(lexer->lookahead)) return false;
        skip_word(lexer);
        break;
      case '[':
        if (!skip_accessor(lexer)) return false;
        break;
      case '<':
        if (!skip_variant(lexer)) return false;
        break;
      case '?':
        // An optional step: the next step directly follows.
        advance(lexer);
        if (lexer->lookahead != '.' && lexer->lookahead != '[') return false;
        break;
      default:
        return false;
    }
  }
}

static bool at_target_and_colon(TSLexer *lexer) {
  if (lexer->lookahead == '"') {
    if (!skip_string(lexer)) return false;
  } else if (lexer->lookahead == '[') {
    if (!skip_accessor(lexer)) return false;
  } else if (is_identifier_char(lexer->lookahead)) {
    if (!skip_word_or_raw_string(lexer)) return false;
  } else {
    return false;
  }
  return at_selectors_and_colon(lexer);
}

// Whether the text at the lexer has the shape of an entry.
static bool at_entry_shape(TSLexer *lexer) {
  switch (lexer->lookahead) {
    case '?':
    case '^':
      return true;
    case '.':
      advance(lexer);
      return lexer->lookahead == '.';
    case '+':
      advance(lexer);
      return skip_inline_trivia(lexer) && at_target_and_colon(lexer);
    case '-':
      advance(lexer);
      // `-?` is never an expression.
      if (lexer->lookahead == '?') return true;
      if (!skip_inline_trivia(lexer)) return false;
      // Not `-12` or `-[key]`.
      return lexer->lookahead == '"' || (is_identifier_char(lexer->lookahead) && !is_digit(lexer->lookahead));
    case '<':
      return skip_variant(lexer) && skip_inline_trivia(lexer) && lexer->lookahead == '{';
    case '#':
      // `#[type(...)]` before a value bind.
      advance(lexer);
      return skip_inline_trivia(lexer) && lexer->lookahead == '[';
    default:
      break;
  }
  return at_target_and_colon(lexer);
}

bool tree_sitter_crol_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  Scanner *scanner = payload;
  if (scanner->line_break_pending) {
    scanner->line_break_pending = false;
    if (valid_symbols[LINE_BREAK]) {
      lexer->mark_end(lexer);
      lexer->result_symbol = LINE_BREAK;
      return true;
    }
  }

  bool line_break = false;
  while (iswspace(lexer->lookahead)) {
    if (lexer->lookahead == '\n') line_break = true;
    lexer->advance(lexer, true);
  }
  lexer->mark_end(lexer);

  bool comment = false;
  if (valid_symbols[BLOCK_COMMENT] && lexer->lookahead == '/') {
    advance(lexer);
    if (lexer->lookahead == '*') {
      advance(lexer);
      if (!skip_block_comment(lexer, &line_break)) return false;
      lexer->mark_end(lexer);
      comment = true;
    } else if (!valid_symbols[LINE_BREAK]) {
      // A line comment, or `/`.
      return false;
    } else if (lexer->lookahead == '/') {
      while (lexer->lookahead != '\n' && !lexer->eof(lexer)) advance(lexer);
    } else {
      // `/`, which only continues a value.
      return false;
    }
  }

  if (valid_symbols[LINE_BREAK] && !comment) {
    if (!skip_trivia(lexer, &line_break)) return false;
    // After `..`, a raw string may be the operand; a word is an identifier.
    if (valid_symbols[RAW_STRING] && lexer->lookahead == 'r') {
      advance(lexer);
      if (lexer->lookahead == '"' || lexer->lookahead == '#') return scan_raw_string(lexer);
      skip_word(lexer);
      if (line_break && at_selectors_and_colon(lexer)) {
        lexer->result_symbol = LINE_BREAK;
        return true;
      }
      return false;
    }
    if (line_break && !lexer->eof(lexer) && at_entry_shape(lexer)) {
      lexer->result_symbol = LINE_BREAK;
      return true;
    }
    return false;
  }
  if (comment) {
    scanner->line_break_pending = valid_symbols[LINE_BREAK] && skip_trivia(lexer, &line_break) &&
                                  line_break && !lexer->eof(lexer) && at_entry_shape(lexer);
    lexer->result_symbol = BLOCK_COMMENT;
    return true;
  }
  if (valid_symbols[RAW_STRING] && lexer->lookahead == 'r') {
    advance(lexer);
    return scan_raw_string(lexer);
  }
  return false;
}
