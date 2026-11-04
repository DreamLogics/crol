// The external scanner of the CROL grammar: raw strings (`r#"..."#`, with any
// number of `#`) and block comments, which nest. Neither is a regular
// language, so the generated lexer cannot match them.

#include "tree_sitter/parser.h"

#include <wctype.h>

enum TokenType {
  RAW_STRING,
  BLOCK_COMMENT,
};

void *tree_sitter_crol_external_scanner_create(void) { return NULL; }

void tree_sitter_crol_external_scanner_destroy(void *payload) { (void)payload; }

unsigned tree_sitter_crol_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_crol_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static bool scan_raw_string(TSLexer *lexer) {
  // At `r`.
  advance(lexer);
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

static bool scan_block_comment(TSLexer *lexer) {
  // At `/`.
  advance(lexer);
  if (lexer->lookahead != '*') return false;
  advance(lexer);
  unsigned depth = 1;
  while (depth > 0) {
    // An unterminated comment is an error, as in the Rust parser.
    if (lexer->eof(lexer)) return false;
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
  lexer->mark_end(lexer);
  lexer->result_symbol = BLOCK_COMMENT;
  return true;
}

bool tree_sitter_crol_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  (void)payload;
  while (iswspace(lexer->lookahead)) lexer->advance(lexer, true);
  if (valid_symbols[RAW_STRING] && lexer->lookahead == 'r') return scan_raw_string(lexer);
  if (valid_symbols[BLOCK_COMMENT] && lexer->lookahead == '/') return scan_block_comment(lexer);
  return false;
}
