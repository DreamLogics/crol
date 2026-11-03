//! Conversion from byte offsets to lines and columns (`SPEC.md` 2.1).

use rowan::TextSize;

/// A position as a line and a column, both counted from 1. Columns count
/// Unicode scalar values; tools that need UTF-16 columns convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineCol {
    /// The line, from 1.
    pub line: u32,
    /// The column, from 1, in Unicode scalar values.
    pub col: u32,
}

/// The start of every line of a text, for converting offsets to positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    /// The offset of the start of each line; the first is 0.
    line_starts: Vec<TextSize>,
}

impl LineIndex {
    /// Indexes `text`. Lines end at `\n`; a `\r` before it belongs to the
    /// line it ends.
    pub fn new(text: &str) -> Self {
        let line_starts = std::iter::once(TextSize::new(0))
            .chain(
                text.match_indices('\n')
                    .map(|(i, _)| TextSize::try_from(i + 1).expect("source text exceeds 4 GiB")),
            )
            .collect();
        Self { line_starts }
    }

    /// The line and column of `offset` in `text`, which must be the text this
    /// index was made from. An offset inside a character counts as that
    /// character.
    pub fn line_col(&self, text: &str, offset: TextSize) -> LineCol {
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        let start = usize::from(self.line_starts[line]);
        let end = usize::from(offset).min(text.len());
        let col = text[start..]
            .char_indices()
            .take_while(|&(i, _)| start + i < end)
            .count();
        LineCol {
            line: u32::try_from(line + 1).expect("too many lines"),
            col: u32::try_from(col + 1).expect("line too long"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions() {
        let text = "ab\r\néx\n\nz";
        let index = LineIndex::new(text);
        let at = |offset: u32| {
            let lc = index.line_col(text, TextSize::new(offset));
            (lc.line, lc.col)
        };
        assert_eq!(at(0), (1, 1));
        assert_eq!(at(2), (1, 3)); // `\r`
        assert_eq!(at(4), (2, 1)); // `é`
        assert_eq!(at(6), (2, 2)); // `x`, after the two bytes of `é`
        assert_eq!(at(8), (3, 1));
        assert_eq!(at(9), (4, 1));
        assert_eq!(at(10), (4, 2)); // the end
    }
}
