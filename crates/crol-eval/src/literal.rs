//! Decoding literal tokens (`SPEC.md` 2.4). The lexer has already checked
//! their syntax, so decoding fails only on values out of range.

/// Decodes an integer token, negated if `negative` (`SPEC.md` 6.2 folds a
/// `-` before a literal into it, so that `i128::MIN` can be written).
pub(crate) fn int(text: &str, negative: bool) -> Result<i128, String> {
    let (digits, radix) = match text.get(..2) {
        Some("0x") => (&text[2..], 16),
        Some("0o") => (&text[2..], 8),
        Some("0b") => (&text[2..], 2),
        _ => (text, 10),
    };
    let digits: String = digits.chars().filter(|&c| c != '_').collect();
    let signed = if negative {
        format!("-{digits}")
    } else {
        digits
    };
    i128::from_str_radix(&signed, radix).map_err(|_| {
        let sign = if negative { "-" } else { "" };
        format!("the integer {sign}{text} does not fit in 128 bits")
    })
}

/// Decodes a float token, negated if `negative`. Parsing is correctly
/// rounded (`SPEC.md` 7.6).
pub(crate) fn float(text: &str, negative: bool) -> Result<f64, String> {
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    let value: f64 = digits
        .parse()
        .map_err(|_| format!("invalid float `{text}`"))?;
    if !value.is_finite() {
        return Err(format!("the float {text} is too large"));
    }
    Ok(if negative { -value } else { value })
}

/// Decodes a string or raw string token.
pub(crate) fn string(text: &str) -> String {
    if let Some(raw) = text.strip_prefix('r') {
        let hashes = raw.bytes().take_while(|&b| b == b'#').count();
        let inner = &raw[hashes..raw.len() - hashes];
        return inner[1..inner.len().saturating_sub(1).max(1)].to_owned();
    }
    let inner = &text[1..text.len().saturating_sub(1).max(1)];
    unescape(inner)
}

/// Decodes a char token.
pub(crate) fn char(text: &str) -> char {
    let inner = &text[1..text.len() - 1];
    unescape(inner).chars().next().unwrap_or('\0')
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('x') => {
                let digits: String = chars.by_ref().take(2).collect();
                if let Some(c) = u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    out.push(c);
                }
            }
            Some('u') => {
                chars.next(); // `{`
                let digits: String = chars
                    .by_ref()
                    .take_while(|&c| c != '}')
                    .filter(|&c| c != '_')
                    .collect();
                if let Some(c) = u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    out.push(c);
                }
            }
            // `\` and a line end: skip the leading whitespace of the next line.
            Some('\r' | '\n') => {
                while chars
                    .next_if(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
                    .is_some()
                {}
            }
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers() {
        assert_eq!(int("12", false), Ok(12));
        assert_eq!(int("0x1F", false), Ok(31));
        assert_eq!(int("0o17", false), Ok(15));
        assert_eq!(int("0b1010", false), Ok(10));
        assert_eq!(int("1_000", true), Ok(-1000));
        assert_eq!(
            int("170141183460469231731687303715884105728", true),
            Ok(i128::MIN)
        );
        assert!(int("170141183460469231731687303715884105728", false).is_err());
    }

    #[test]
    fn floats() {
        assert_eq!(float("1.5", false), Ok(1.5));
        assert_eq!(float("2.5E-3", true), Ok(-0.0025));
        assert_eq!(float("1_000.0", false), Ok(1000.0));
        assert!(float("1e400", false).is_err());
    }

    #[test]
    fn strings() {
        assert_eq!(string(r#""a\n\"\u{1F600}\x41""#), "a\n\"😀A");
        assert_eq!(string("\"skip \\\n    ws\""), "skip ws");
        assert_eq!(string(r##"r#"a "q" b"#"##), "a \"q\" b");
        assert_eq!(string(r#"r"raw\n""#), "raw\\n");
        assert_eq!(char(r"'\u{E9}'"), 'é');
        assert_eq!(char("'a'"), 'a');
    }
}
