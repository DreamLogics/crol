//! Tree snapshots for the parser. Run with `UPDATE_EXPECT=1` to update them,
//! then review the diff.

use expect_test::{Expect, expect};

fn check(text: &str, expect: Expect) {
    let parse = crol_syntax::parse(text);
    assert_eq!(
        parse.syntax().to_string(),
        text,
        "the tree must be lossless"
    );
    let mut out = parse.debug_tree();
    for error in parse.errors() {
        out.push_str(&format!("error {:?}: {}\n", error.range(), error.message()));
    }
    expect.assert_eq(&out);
}

#[test]
fn data_mode_entries() {
    check(
        "+a: 1, ?b: 2, c.d: 3, -e,",
        expect![[r#"
            DOCUMENT@0..25
              ENTRY@0..5
                PLUS@0..1 "+"
                TARGET@1..2
                  KEY@1..2
                    IDENT@1..2 "a"
                COLON@2..3 ":"
                WHITESPACE@3..4 " "
                LITERAL@4..5
                  INT@4..5 "1"
              COMMA@5..6 ","
              WHITESPACE@6..7 " "
              ENTRY@7..12
                QUESTION@7..8 "?"
                TARGET@8..9
                  KEY@8..9
                    IDENT@8..9 "b"
                COLON@9..10 ":"
                WHITESPACE@10..11 " "
                LITERAL@11..12
                  INT@11..12 "2"
              COMMA@12..13 ","
              WHITESPACE@13..14 " "
              ENTRY@14..20
                TARGET@14..17
                  KEY@14..15
                    IDENT@14..15 "c"
                  FIELD_SELECTOR@15..17
                    DOT@15..16 "."
                    IDENT@16..17 "d"
                COLON@17..18 ":"
                WHITESPACE@18..19 " "
                LITERAL@19..20
                  INT@19..20 "3"
              COMMA@20..21 ","
              WHITESPACE@21..22 " "
              REMOVE_ENTRY@22..24
                MINUS@22..23 "-"
                TARGET@23..24
                  KEY@23..24
                    IDENT@23..24 "e"
              COMMA@24..25 ","
        "#]],
    );
}

#[test]
fn value_mode_expression() {
    check(
        "Service { port: 80, .. }",
        expect![[r#"
            DOCUMENT@0..24
              BLOCK@0..24
                IDENT@0..7 "Service"
                WHITESPACE@7..8 " "
                L_BRACE@8..9 "{"
                WHITESPACE@9..10 " "
                ENTRY@10..18
                  TARGET@10..14
                    KEY@10..14
                      IDENT@10..14 "port"
                  COLON@14..15 ":"
                  WHITESPACE@15..16 " "
                  LITERAL@16..18
                    INT@16..18 "80"
                COMMA@18..19 ","
                WHITESPACE@19..20 " "
                DEFAULTS@20..22
                  DOT2@20..22 ".."
                WHITESPACE@22..23 " "
                R_BRACE@23..24 "}"
        "#]],
    );
}

#[test]
fn operator_precedence() {
    // `|` is loosest, then `~`, then `+`, then `*`; `-` before a number is
    // a prefix.
    check(
        "x: & ~ { a: 1 } | -2 * 3 + 4",
        expect![[r#"
            DOCUMENT@0..28
              ENTRY@0..28
                TARGET@0..1
                  KEY@0..1
                    IDENT@0..1 "x"
                COLON@1..2 ":"
                WHITESPACE@2..3 " "
                BINARY_EXPR@3..28
                  BINARY_EXPR@3..15
                    REFERENCE@3..4
                      AMP@3..4 "&"
                    WHITESPACE@4..5 " "
                    TILDE@5..6 "~"
                    WHITESPACE@6..7 " "
                    BLOCK@7..15
                      L_BRACE@7..8 "{"
                      WHITESPACE@8..9 " "
                      ENTRY@9..13
                        TARGET@9..10
                          KEY@9..10
                            IDENT@9..10 "a"
                        COLON@10..11 ":"
                        WHITESPACE@11..12 " "
                        LITERAL@12..13
                          INT@12..13 "1"
                      WHITESPACE@13..14 " "
                      R_BRACE@14..15 "}"
                  WHITESPACE@15..16 " "
                  PIPE@16..17 "|"
                  WHITESPACE@17..18 " "
                  BINARY_EXPR@18..28
                    BINARY_EXPR@18..24
                      PREFIX_EXPR@18..20
                        MINUS@18..19 "-"
                        LITERAL@19..20
                          INT@19..20 "2"
                      WHITESPACE@20..21 " "
                      STAR@21..22 "*"
                      WHITESPACE@22..23 " "
                      LITERAL@23..24
                        INT@23..24 "3"
                    WHITESPACE@24..25 " "
                    PLUS@25..26 "+"
                    WHITESPACE@26..27 " "
                    LITERAL@27..28
                      INT@27..28 "4"
        "#]],
    );
}

#[test]
fn references() {
    check(
        "a: &self.port + &parent.&parent.x + &root.web<prod> + &tpl + &default + &-1",
        expect![[r#"
            DOCUMENT@0..75
              ENTRY@0..75
                TARGET@0..1
                  KEY@0..1
                    IDENT@0..1 "a"
                COLON@1..2 ":"
                WHITESPACE@2..3 " "
                BINARY_EXPR@3..75
                  BINARY_EXPR@3..73
                    BINARY_EXPR@3..69
                      BINARY_EXPR@3..58
                        BINARY_EXPR@3..51
                          BINARY_EXPR@3..33
                            PATH_EXPR@3..13
                              REFERENCE@3..8
                                AMP@3..4 "&"
                                SELF_KW@4..8 "self"
                              FIELD_SELECTOR@8..13
                                DOT@8..9 "."
                                IDENT@9..13 "port"
                            WHITESPACE@13..14 " "
                            PLUS@14..15 "+"
                            WHITESPACE@15..16 " "
                            PATH_EXPR@16..33
                              REFERENCE@16..31
                                AMP@16..17 "&"
                                PARENT_KW@17..23 "parent"
                                DOT@23..24 "."
                                AMP@24..25 "&"
                                PARENT_KW@25..31 "parent"
                              FIELD_SELECTOR@31..33
                                DOT@31..32 "."
                                IDENT@32..33 "x"
                          WHITESPACE@33..34 " "
                          PLUS@34..35 "+"
                          WHITESPACE@35..36 " "
                          PATH_EXPR@36..51
                            REFERENCE@36..41
                              AMP@36..37 "&"
                              ROOT_KW@37..41 "root"
                            FIELD_SELECTOR@41..45
                              DOT@41..42 "."
                              IDENT@42..45 "web"
                            VARIANT@45..51
                              L_ANGLE@45..46 "<"
                              COND_NAME@46..50
                                IDENT@46..50 "prod"
                              R_ANGLE@50..51 ">"
                        WHITESPACE@51..52 " "
                        PLUS@52..53 "+"
                        WHITESPACE@53..54 " "
                        REFERENCE@54..58
                          AMP@54..55 "&"
                          IDENT@55..58 "tpl"
                      WHITESPACE@58..59 " "
                      PLUS@59..60 "+"
                      WHITESPACE@60..61 " "
                      REFERENCE@61..69
                        AMP@61..62 "&"
                        DEFAULT_KW@62..69 "default"
                    WHITESPACE@69..70 " "
                    PLUS@70..71 "+"
                    WHITESPACE@71..72 " "
                    REFERENCE@72..73
                      AMP@72..73 "&"
                  MINUS@73..74 "-"
                  LITERAL@74..75
                    INT@74..75 "1"
        "#]],
    );
}

#[test]
fn variants() {
    check(
        "<prod & !cache> { web.replicas<a + (b | c)>: 1 }, x: &root.db<dev & !cache>,",
        expect![[r#"
            DOCUMENT@0..76
              VARIANT_BLOCK@0..48
                VARIANT@0..15
                  L_ANGLE@0..1 "<"
                  COND_BINARY@1..14
                    COND_NAME@1..5
                      IDENT@1..5 "prod"
                    WHITESPACE@5..6 " "
                    AMP@6..7 "&"
                    WHITESPACE@7..8 " "
                    COND_NOT@8..14
                      BANG@8..9 "!"
                      COND_NAME@9..14
                        IDENT@9..14 "cache"
                  R_ANGLE@14..15 ">"
                WHITESPACE@15..16 " "
                L_BRACE@16..17 "{"
                WHITESPACE@17..18 " "
                ENTRY@18..46
                  TARGET@18..43
                    KEY@18..21
                      IDENT@18..21 "web"
                    FIELD_SELECTOR@21..30
                      DOT@21..22 "."
                      IDENT@22..30 "replicas"
                    VARIANT@30..43
                      L_ANGLE@30..31 "<"
                      COND_NAME@31..32
                        IDENT@31..32 "a"
                      WHITESPACE@32..33 " "
                      PLUS@33..34 "+"
                      WHITESPACE@34..35 " "
                      COND_PAREN@35..42
                        L_PAREN@35..36 "("
                        COND_BINARY@36..41
                          COND_NAME@36..37
                            IDENT@36..37 "b"
                          WHITESPACE@37..38 " "
                          PIPE@38..39 "|"
                          WHITESPACE@39..40 " "
                          COND_NAME@40..41
                            IDENT@40..41 "c"
                        R_PAREN@41..42 ")"
                      R_ANGLE@42..43 ">"
                  COLON@43..44 ":"
                  WHITESPACE@44..45 " "
                  LITERAL@45..46
                    INT@45..46 "1"
                WHITESPACE@46..47 " "
                R_BRACE@47..48 "}"
              COMMA@48..49 ","
              WHITESPACE@49..50 " "
              ENTRY@50..75
                TARGET@50..51
                  KEY@50..51
                    IDENT@50..51 "x"
                COLON@51..52 ":"
                WHITESPACE@52..53 " "
                PATH_EXPR@53..75
                  REFERENCE@53..58
                    AMP@53..54 "&"
                    ROOT_KW@54..58 "root"
                  FIELD_SELECTOR@58..61
                    DOT@58..59 "."
                    IDENT@59..61 "db"
                  VARIANT@61..75
                    L_ANGLE@61..62 "<"
                    COND_BINARY@62..74
                      COND_NAME@62..65
                        IDENT@62..65 "dev"
                      WHITESPACE@65..66 " "
                      AMP@66..67 "&"
                      WHITESPACE@67..68 " "
                      COND_NOT@68..74
                        BANG@68..69 "!"
                        COND_NAME@69..74
                          IDENT@69..74 "cache"
                    R_ANGLE@74..75 ">"
              COMMA@75..76 ","
        "#]],
    );
}

#[test]
fn ids_tuples_and_named_tuples() {
    check(
        r#"a: [@web, @"odd id", (), (1), (1,), Some(1), Rgb(1, 2, 3).0, None]"#,
        expect![[r#"
            DOCUMENT@0..66
              ENTRY@0..66
                TARGET@0..1
                  KEY@0..1
                    IDENT@0..1 "a"
                COLON@1..2 ":"
                WHITESPACE@2..3 " "
                LIST_EXPR@3..66
                  L_BRACK@3..4 "["
                  ID_EXPR@4..8
                    AT@4..5 "@"
                    IDENT@5..8 "web"
                  COMMA@8..9 ","
                  WHITESPACE@9..10 " "
                  ID_EXPR@10..19
                    AT@10..11 "@"
                    STRING@11..19 "\"odd id\""
                  COMMA@19..20 ","
                  WHITESPACE@20..21 " "
                  TUPLE_EXPR@21..23
                    L_PAREN@21..22 "("
                    R_PAREN@22..23 ")"
                  COMMA@23..24 ","
                  WHITESPACE@24..25 " "
                  PAREN_EXPR@25..28
                    L_PAREN@25..26 "("
                    LITERAL@26..27
                      INT@26..27 "1"
                    R_PAREN@27..28 ")"
                  COMMA@28..29 ","
                  WHITESPACE@29..30 " "
                  TUPLE_EXPR@30..34
                    L_PAREN@30..31 "("
                    LITERAL@31..32
                      INT@31..32 "1"
                    COMMA@32..33 ","
                    R_PAREN@33..34 ")"
                  COMMA@34..35 ","
                  WHITESPACE@35..36 " "
                  NAMED_TUPLE@36..43
                    IDENT@36..40 "Some"
                    ARG_LIST@40..43
                      L_PAREN@40..41 "("
                      LITERAL@41..42
                        INT@41..42 "1"
                      R_PAREN@42..43 ")"
                  COMMA@43..44 ","
                  WHITESPACE@44..45 " "
                  PATH_EXPR@45..59
                    NAMED_TUPLE@45..57
                      IDENT@45..48 "Rgb"
                      ARG_LIST@48..57
                        L_PAREN@48..49 "("
                        LITERAL@49..50
                          INT@49..50 "1"
                        COMMA@50..51 ","
                        WHITESPACE@51..52 " "
                        LITERAL@52..53
                          INT@52..53 "2"
                        COMMA@53..54 ","
                        WHITESPACE@54..55 " "
                        LITERAL@55..56
                          INT@55..56 "3"
                        R_PAREN@56..57 ")"
                    INDEX_SELECTOR@57..59
                      DOT@57..58 "."
                      INT@58..59 "0"
                  COMMA@59..60 ","
                  WHITESPACE@60..61 " "
                  UNIT_NAME@61..65
                    IDENT@61..65 "None"
                  R_BRACK@65..66 "]"
        "#]],
    );
}

#[test]
fn binds_and_spreads() {
    check(
        "t := { port = port: 80, ..&base, .. }, +web: Service { ..&t, }",
        expect![[r#"
            DOCUMENT@0..62
              VALUE_BIND@0..37
                IDENT@0..1 "t"
                WHITESPACE@1..2 " "
                COLON_EQ@2..4 ":="
                WHITESPACE@4..5 " "
                BLOCK@5..37
                  L_BRACE@5..6 "{"
                  WHITESPACE@6..7 " "
                  ENTRY@7..22
                    BIND@7..13
                      IDENT@7..11 "port"
                      WHITESPACE@11..12 " "
                      EQ@12..13 "="
                    WHITESPACE@13..14 " "
                    TARGET@14..18
                      KEY@14..18
                        IDENT@14..18 "port"
                    COLON@18..19 ":"
                    WHITESPACE@19..20 " "
                    LITERAL@20..22
                      INT@20..22 "80"
                  COMMA@22..23 ","
                  WHITESPACE@23..24 " "
                  SPREAD@24..31
                    DOT2@24..26 ".."
                    REFERENCE@26..31
                      AMP@26..27 "&"
                      IDENT@27..31 "base"
                  COMMA@31..32 ","
                  WHITESPACE@32..33 " "
                  DEFAULTS@33..35
                    DOT2@33..35 ".."
                  WHITESPACE@35..36 " "
                  R_BRACE@36..37 "}"
              COMMA@37..38 ","
              WHITESPACE@38..39 " "
              ENTRY@39..62
                PLUS@39..40 "+"
                TARGET@40..43
                  KEY@40..43
                    IDENT@40..43 "web"
                COLON@43..44 ":"
                WHITESPACE@44..45 " "
                BLOCK@45..62
                  IDENT@45..52 "Service"
                  WHITESPACE@52..53 " "
                  L_BRACE@53..54 "{"
                  WHITESPACE@54..55 " "
                  SPREAD@55..59
                    DOT2@55..57 ".."
                    REFERENCE@57..59
                      AMP@57..58 "&"
                      IDENT@58..59 "t"
                  COMMA@59..60 ","
                  WHITESPACE@60..61 " "
                  R_BRACE@61..62 "}"
        "#]],
    );
}

#[test]
fn accessors_and_negative_keys() {
    check(
        r#"m: { [-3]: "a", 12: "b", "x y": 1 }, -m[-3],"#,
        expect![[r#"
        DOCUMENT@0..44
          ENTRY@0..35
            TARGET@0..1
              KEY@0..1
                IDENT@0..1 "m"
            COLON@1..2 ":"
            WHITESPACE@2..3 " "
            BLOCK@3..35
              L_BRACE@3..4 "{"
              WHITESPACE@4..5 " "
              ENTRY@5..14
                TARGET@5..9
                  KEY@5..9
                    ACCESSOR@5..9
                      L_BRACK@5..6 "["
                      MINUS@6..7 "-"
                      INT@7..8 "3"
                      R_BRACK@8..9 "]"
                COLON@9..10 ":"
                WHITESPACE@10..11 " "
                LITERAL@11..14
                  STRING@11..14 "\"a\""
              COMMA@14..15 ","
              WHITESPACE@15..16 " "
              ENTRY@16..23
                TARGET@16..18
                  KEY@16..18
                    INT@16..18 "12"
                COLON@18..19 ":"
                WHITESPACE@19..20 " "
                LITERAL@20..23
                  STRING@20..23 "\"b\""
              COMMA@23..24 ","
              WHITESPACE@24..25 " "
              ENTRY@25..33
                TARGET@25..30
                  KEY@25..30
                    STRING@25..30 "\"x y\""
                COLON@30..31 ":"
                WHITESPACE@31..32 " "
                LITERAL@32..33
                  INT@32..33 "1"
              WHITESPACE@33..34 " "
              R_BRACE@34..35 "}"
          COMMA@35..36 ","
          WHITESPACE@36..37 " "
          REMOVE_ENTRY@37..43
            MINUS@37..38 "-"
            TARGET@38..43
              KEY@38..39
                IDENT@38..39 "m"
              ACCESSOR@39..43
                L_BRACK@39..40 "["
                MINUS@40..41 "-"
                INT@41..42 "3"
                R_BRACK@42..43 "]"
          COMMA@43..44 ","
    "#]],
    );
}

#[test]
fn attributes() {
    check(
        "#![use(app::net::Config, app::Pair<u32, List(String)> as P)]\n\
         #![schema(\"crate:app@2.1\")]\n\
         #![extends(super::super::defaults::services)]\n\
         #![type(Config)]\n\
         port := 8080,",
        expect![[r##"
            DOCUMENT@0..165
              ATTRIBUTE@0..60
                POUND@0..1 "#"
                BANG@1..2 "!"
                L_BRACK@2..3 "["
                USE_ATTR@3..59
                  USE_KW@3..6 "use"
                  L_PAREN@6..7 "("
                  USE_PATH@7..23
                    IDENT@7..10 "app"
                    COLON2@10..12 "::"
                    IDENT@12..15 "net"
                    COLON2@15..17 "::"
                    IDENT@17..23 "Config"
                  COMMA@23..24 ","
                  WHITESPACE@24..25 " "
                  USE_PATH@25..58
                    IDENT@25..28 "app"
                    COLON2@28..30 "::"
                    IDENT@30..34 "Pair"
                    TYPE_ARG_LIST@34..53
                      L_ANGLE@34..35 "<"
                      TYPE_EXPR@35..38
                        IDENT@35..38 "u32"
                      COMMA@38..39 ","
                      WHITESPACE@39..40 " "
                      TYPE_EXPR@40..52
                        IDENT@40..44 "List"
                        TYPE_PAREN_LIST@44..52
                          L_PAREN@44..45 "("
                          TYPE_EXPR@45..51
                            IDENT@45..51 "String"
                          R_PAREN@51..52 ")"
                      R_ANGLE@52..53 ">"
                    WHITESPACE@53..54 " "
                    USE_ALIAS@54..58
                      AS_KW@54..56 "as"
                      WHITESPACE@56..57 " "
                      IDENT@57..58 "P"
                  R_PAREN@58..59 ")"
                R_BRACK@59..60 "]"
              WHITESPACE@60..61 "\n"
              ATTRIBUTE@61..88
                POUND@61..62 "#"
                BANG@62..63 "!"
                L_BRACK@63..64 "["
                SCHEMA_ATTR@64..87
                  SCHEMA_KW@64..70 "schema"
                  L_PAREN@70..71 "("
                  STRING@71..86 "\"crate:app@2.1\""
                  R_PAREN@86..87 ")"
                R_BRACK@87..88 "]"
              WHITESPACE@88..89 "\n"
              ATTRIBUTE@89..134
                POUND@89..90 "#"
                BANG@90..91 "!"
                L_BRACK@91..92 "["
                EXTENDS_ATTR@92..133
                  EXTENDS_KW@92..99 "extends"
                  L_PAREN@99..100 "("
                  MODULE_PATH@100..132
                    SUPER_KW@100..105 "super"
                    COLON2@105..107 "::"
                    SUPER_KW@107..112 "super"
                    COLON2@112..114 "::"
                    IDENT@114..122 "defaults"
                    COLON2@122..124 "::"
                    IDENT@124..132 "services"
                  R_PAREN@132..133 ")"
                R_BRACK@133..134 "]"
              WHITESPACE@134..135 "\n"
              ATTRIBUTE@135..151
                POUND@135..136 "#"
                BANG@136..137 "!"
                L_BRACK@137..138 "["
                TYPE_ATTR@138..150
                  TYPE_KW@138..142 "type"
                  L_PAREN@142..143 "("
                  IDENT@143..149 "Config"
                  R_PAREN@149..150 ")"
                R_BRACK@150..151 "]"
              WHITESPACE@151..152 "\n"
              VALUE_BIND@152..164
                IDENT@152..156 "port"
                WHITESPACE@156..157 " "
                COLON_EQ@157..159 ":="
                WHITESPACE@159..160 " "
                LITERAL@160..164
                  INT@160..164 "8080"
              COMMA@164..165 ","
        "##]],
    );
}

// ---- Errors and recovery ----

#[test]
fn missing_separator_between_entries() {
    check(
        "a: 1 b: 2",
        expect![[r#"
            DOCUMENT@0..9
              ENTRY@0..4
                TARGET@0..1
                  KEY@0..1
                    IDENT@0..1 "a"
                COLON@1..2 ":"
                WHITESPACE@2..3 " "
                LITERAL@3..4
                  INT@3..4 "1"
              WHITESPACE@4..5 " "
              ENTRY@5..9
                TARGET@5..6
                  KEY@5..6
                    IDENT@5..6 "b"
                COLON@6..7 ":"
                WHITESPACE@7..8 " "
                LITERAL@8..9
                  INT@8..9 "2"
            error 4..4: expected `,` or a line break between entries
        "#]],
    );
}

#[test]
fn unclosed_block() {
    check(
        "+web: Service { port: 80,\n+db: 1,",
        expect![[r#"
        DOCUMENT@0..33
          ENTRY@0..33
            PLUS@0..1 "+"
            TARGET@1..4
              KEY@1..4
                IDENT@1..4 "web"
            COLON@4..5 ":"
            WHITESPACE@5..6 " "
            BLOCK@6..33
              IDENT@6..13 "Service"
              WHITESPACE@13..14 " "
              L_BRACE@14..15 "{"
              WHITESPACE@15..16 " "
              ENTRY@16..24
                TARGET@16..20
                  KEY@16..20
                    IDENT@16..20 "port"
                COLON@20..21 ":"
                WHITESPACE@21..22 " "
                LITERAL@22..24
                  INT@22..24 "80"
              COMMA@24..25 ","
              WHITESPACE@25..26 "\n"
              ENTRY@26..32
                PLUS@26..27 "+"
                TARGET@27..29
                  KEY@27..29
                    IDENT@27..29 "db"
                COLON@29..30 ":"
                WHITESPACE@30..31 " "
                LITERAL@31..32
                  INT@31..32 "1"
              COMMA@32..33 ","
        error 33..33: expected `}`
    "#]],
    );
}

#[test]
fn adjacency_errors() {
    check(
        "a: & port + @ web,",
        expect![[r#"
        DOCUMENT@0..18
          ENTRY@0..17
            TARGET@0..1
              KEY@0..1
                IDENT@0..1 "a"
            COLON@1..2 ":"
            WHITESPACE@2..3 " "
            BINARY_EXPR@3..17
              REFERENCE@3..9
                AMP@3..4 "&"
                WHITESPACE@4..5 " "
                IDENT@5..9 "port"
              WHITESPACE@9..10 " "
              PLUS@10..11 "+"
              WHITESPACE@11..12 " "
              ID_EXPR@12..17
                AT@12..13 "@"
                WHITESPACE@13..14 " "
                IDENT@14..17 "web"
          COMMA@17..18 ","
        error 5..9: a reference name must follow `&` directly
        error 14..17: an id's name must follow `@` directly
    "#]],
    );
}

#[test]
fn identifier_in_brackets() {
    check(
        "a: &self[port],",
        expect![[r#"
        DOCUMENT@0..15
          ENTRY@0..14
            TARGET@0..1
              KEY@0..1
                IDENT@0..1 "a"
            COLON@1..2 ":"
            WHITESPACE@2..3 " "
            PATH_EXPR@3..14
              REFERENCE@3..8
                AMP@3..4 "&"
                SELF_KW@4..8 "self"
              ACCESSOR@8..14
                L_BRACK@8..9 "["
                IDENT@9..13 "port"
                R_BRACK@13..14 "]"
          COMMA@14..15 ","
        error 9..13: an identifier in brackets is not a lookup; write `.port`, or `["port"]` for the key
    "#]],
    );
}

#[test]
fn misplaced_parent_and_plus_list() {
    check(
        "a: &self.&parent, b<(x + y)>: 1,",
        expect![[r#"
        DOCUMENT@0..32
          ENTRY@0..16
            TARGET@0..1
              KEY@0..1
                IDENT@0..1 "a"
            COLON@1..2 ":"
            WHITESPACE@2..3 " "
            PATH_EXPR@3..16
              REFERENCE@3..8
                AMP@3..4 "&"
                SELF_KW@4..8 "self"
              PARENT_SELECTOR@8..16
                DOT@8..9 "."
                AMP@9..10 "&"
                PARENT_KW@10..16 "parent"
          COMMA@16..17 ","
          WHITESPACE@17..18 " "
          ENTRY@18..31
            TARGET@18..28
              KEY@18..19
                IDENT@18..19 "b"
              VARIANT@19..28
                L_ANGLE@19..20 "<"
                COND_PAREN@20..27
                  L_PAREN@20..21 "("
                  COND_NAME@21..22
                    IDENT@21..22 "x"
                  WHITESPACE@22..23 " "
                  PLUS@23..24 "+"
                  WHITESPACE@24..25 " "
                  COND_NAME@25..26
                    IDENT@25..26 "y"
                  R_PAREN@26..27 ")"
                R_ANGLE@27..28 ">"
            COLON@28..29 ":"
            WHITESPACE@29..30 " "
            LITERAL@30..31
              INT@30..31 "1"
          COMMA@31..32 ","
        error 8..9: `.&parent` is only allowed directly after `&parent`
        error 23..24: a `+` list is only allowed as the outermost operator of a condition
    "#]],
    );
}

#[test]
fn reserved_words_and_garbage() {
    check(
        "true: 1, $ x: 2, y: }",
        expect![[r#"
        DOCUMENT@0..21
          ENTRY@0..7
            TARGET@0..4
              KEY@0..4
                TRUE_KW@0..4 "true"
            COLON@4..5 ":"
            WHITESPACE@5..6 " "
            LITERAL@6..7
              INT@6..7 "1"
          COMMA@7..8 ","
          WHITESPACE@8..9 " "
          ERROR@9..10
            ERROR_TOKEN@9..10 "$"
          WHITESPACE@10..11 " "
          ENTRY@11..15
            TARGET@11..12
              KEY@11..12
                IDENT@11..12 "x"
            COLON@12..13 ":"
            WHITESPACE@13..14 " "
            LITERAL@14..15
              INT@14..15 "2"
          COMMA@15..16 ","
          WHITESPACE@16..17 " "
          ENTRY@17..20
            TARGET@17..18
              KEY@17..18
                IDENT@17..18 "y"
            COLON@18..19 ":"
            WHITESPACE@19..20 " "
          ERROR@20..21
            R_BRACE@20..21 "}"
        error 0..4: `true` is reserved; write the key as `"true"`
        error 9..10: unexpected character `$`
        error 19..19: expected an expression
        error 20..21: unexpected `}`
    "#]],
    );
}

#[test]
fn attribute_errors() {
    check(
        "#[derive(X)]\n#![nope]\na: 1,\n#![type(T)]",
        expect![[r##"
        DOCUMENT@0..39
          ATTRIBUTE@0..12
            POUND@0..1 "#"
            L_BRACK@1..2 "["
            ERROR@2..11
              IDENT@2..8 "derive"
              L_PAREN@8..9 "("
              IDENT@9..10 "X"
              R_PAREN@10..11 ")"
            R_BRACK@11..12 "]"
          WHITESPACE@12..13 "\n"
          ATTRIBUTE@13..21
            POUND@13..14 "#"
            BANG@14..15 "!"
            L_BRACK@15..16 "["
            ERROR@16..20
              IDENT@16..20 "nope"
            R_BRACK@20..21 "]"
          WHITESPACE@21..22 "\n"
          ENTRY@22..26
            TARGET@22..23
              KEY@22..23
                IDENT@22..23 "a"
            COLON@23..24 ":"
            WHITESPACE@24..25 " "
            LITERAL@25..26
              INT@25..26 "1"
          COMMA@26..27 ","
          WHITESPACE@27..28 "\n"
          ATTRIBUTE@28..39
            POUND@28..29 "#"
            BANG@29..30 "!"
            L_BRACK@30..31 "["
            TYPE_ATTR@31..38
              TYPE_KW@31..35 "type"
              L_PAREN@35..36 "("
              IDENT@36..37 "T"
              R_PAREN@37..38 ")"
            R_BRACK@38..39 "]"
        error 1..1: expected `!` after `#`: attributes are written `#![...]`
        error 2..8: unknown attribute `derive`; the attributes are `use`, `schema`, `type` and `extends`
        error 16..20: unknown attribute `nope`; the attributes are `use`, `schema`, `type` and `extends`
        error 28..29: attributes must come before the first entry
    "##]],
    );
}
