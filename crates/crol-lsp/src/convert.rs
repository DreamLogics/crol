//! Conversions between LSP's positions and URIs and the syntax tree's
//! offsets and file paths.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use crol_syntax::{LineIndex, TextRange, TextSize};
use lsp_types::{Position, Range, Uri};

/// How the client counts columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16,
}

/// The line and column of an offset, as the client counts columns.
pub(crate) fn position(
    text: &str,
    index: &LineIndex,
    offset: TextSize,
    encoding: Encoding,
) -> Position {
    let line_col = index.line_col(text, offset);
    let line = line_col.line - 1;
    let start = line_start(text, line);
    let end = usize::from(offset).min(text.len());
    let prefix = &text[start..end.max(start)];
    let character = match encoding {
        Encoding::Utf8 => prefix.len(),
        Encoding::Utf16 => prefix.encode_utf16().count(),
    };
    Position::new(line, character as u32)
}

pub(crate) fn range(text: &str, index: &LineIndex, range: TextRange, encoding: Encoding) -> Range {
    Range::new(
        position(text, index, range.start(), encoding),
        position(text, index, range.end(), encoding),
    )
}

/// The byte offset of the start of line `line` (from 0).
fn line_start(text: &str, line: u32) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line as usize - 1)
        .map_or(text.len(), |(i, _)| i + 1)
}

/// The offset of a position.
pub(crate) fn offset(text: &str, position: Position, encoding: Encoding) -> TextSize {
    let start = line_start(text, position.line);
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let line = &text[start..line_end];
    let mut units = 0usize;
    let mut byte = line.len();
    for (i, c) in line.char_indices() {
        if units >= position.character as usize {
            byte = i;
            break;
        }
        units += match encoding {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        };
    }
    TextSize::try_from(start + byte).unwrap_or_default()
}

/// The path of a `file:` URI.
pub(crate) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let rest = text.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    // `file:///C:/x` on Windows.
    let decoded = match decoded.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() && cfg!(windows) => {
            decoded[1..].to_owned()
        }
        _ => decoded,
    };
    Some(PathBuf::from(decoded))
}

/// The `file:` URI of a path.
pub(crate) fn path_to_uri(path: &Path) -> Option<Uri> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    Uri::from_str(&uri).ok()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_in_both_encodings() {
        let text = "a: \"é😀\",\nb: 1,";
        let index = LineIndex::new(text);
        let offset_b = TextSize::from(text.find('b').unwrap() as u32);
        let at_b = position(text, &index, offset_b, Encoding::Utf16);
        assert_eq!(at_b, Position::new(1, 0));
        let end = TextSize::from(text.find(',').unwrap() as u32);
        // `😀` is two UTF-16 code units and four bytes; `é` one and two.
        assert_eq!(
            position(text, &index, end, Encoding::Utf16),
            Position::new(0, 8)
        );
        assert_eq!(
            position(text, &index, end, Encoding::Utf8),
            Position::new(0, 11)
        );
        assert_eq!(offset(text, Position::new(0, 8), Encoding::Utf16), end);
        assert_eq!(offset(text, Position::new(0, 11), Encoding::Utf8), end);
    }

    #[test]
    fn uris() {
        let uri = Uri::from_str("file:///tmp/a%20b/c.crol").unwrap();
        assert_eq!(uri_to_path(&uri), Some(PathBuf::from("/tmp/a b/c.crol")));
    }
}
