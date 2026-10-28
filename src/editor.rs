//! A multi-line prompt buffer with word wrapping.
//!
//! The cursor is a byte offset that always sits on a grapheme boundary.
//! Wrapping is computed on demand for a given width, so the buffer never has
//! to be told about resizes.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One visual row: a byte range of the text, without its newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub struct Editor {
    text: String,
    cursor: usize,
    /// The column up/down movement tries to return to, kept across short rows.
    goal_col: Option<usize>,
    /// First visual row shown; adjusted by the renderer to keep the cursor visible.
    pub scroll: usize,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn slice(&self, row: Row) -> &str {
        &self.text[row.start..row.end]
    }

    pub fn insert(&mut self, s: &str) {
        let s = clean(s);
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.goal_col = None;
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary() {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary() {
            self.text.replace_range(self.cursor..next, "");
        }
        self.goal_col = None;
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary().unwrap_or(0);
        self.goal_col = None;
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary().unwrap_or(self.text.len());
        self.goal_col = None;
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start_before();
        self.goal_col = None;
    }

    pub fn word_right(&mut self) {
        let after = &self.text[self.cursor..];
        let gap = after.len() - after.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let rest = &after[gap..];
        let word = rest.len() - rest.trim_start_matches(char::is_alphanumeric).len();
        self.cursor += gap + word;
        self.goal_col = None;
    }

    pub fn delete_word_left(&mut self) {
        let start = self.word_start_before();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.goal_col = None;
    }

    pub fn line_start(&mut self) {
        self.cursor = self.logical_line_start();
        self.goal_col = None;
    }

    pub fn line_end(&mut self) {
        self.cursor = self.logical_line_end();
        self.goal_col = None;
    }

    pub fn kill_to_line_start(&mut self) {
        let start = self.logical_line_start();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.goal_col = None;
    }

    pub fn kill_to_line_end(&mut self) {
        let end = self.logical_line_end();
        // At the end of a line, join it with the next one, as readline does.
        let end = if end == self.cursor && end < self.text.len() { end + 1 } else { end };
        self.text.replace_range(self.cursor..end, "");
        self.goal_col = None;
    }

    /// Moves the cursor `delta` visual rows, keeping its column where it can.
    pub fn move_vertical(&mut self, width: usize, delta: isize) {
        let rows = self.rows(width);
        let row = row_of(&rows, self.cursor);
        let col = self
            .goal_col
            .unwrap_or_else(|| self.text[rows[row].start..self.cursor].width());
        let target = row as isize + delta;
        if target < 0 {
            self.cursor = 0;
            self.goal_col = None;
            return;
        }
        let target = target as usize;
        if target >= rows.len() {
            self.cursor = self.text.len();
            self.goal_col = None;
            return;
        }
        let soft_wrapped = rows.get(target + 1).is_some_and(|next| next.start == rows[target].end);
        self.cursor = self.index_at_col(rows[target], col, soft_wrapped);
        self.goal_col = Some(col);
    }

    /// Splits the text into visual rows no wider than `width` columns,
    /// breaking after whitespace where possible.
    pub fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in self.text.split('\n') {
            wrap_line(line, line_start, width, &mut rows);
            line_start += line.len() + 1;
        }
        rows
    }

    /// The cursor's visual (row, column) at `width`.
    pub fn cursor_pos(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        let row = row_of(&rows, self.cursor);
        (row, self.text[rows[row].start..self.cursor].width())
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map(|g| self.cursor + g.len())
    }

    fn word_start_before(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric());
        trimmed.trim_end_matches(char::is_alphanumeric).len()
    }

    fn logical_line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn logical_line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    /// The byte offset in `row` closest to column `col`. On a soft-wrapped row
    /// the offset at its very end belongs to the next row, so stop one
    /// grapheme short of it.
    fn index_at_col(&self, row: Row, col: usize, soft_wrapped: bool) -> usize {
        let mut used = 0;
        let mut last = row.start;
        for (i, g) in self.text[row.start..row.end].grapheme_indices(true) {
            let w = g.width();
            if used + w > col {
                return row.start + i;
            }
            used += w;
            last = row.start + i;
        }
        if soft_wrapped && row.end > row.start { last } else { row.end }
    }
}

/// Normalizes pasted or typed text: Windows and old Mac line endings become
/// newlines, tabs become spaces, and other control characters are dropped.
fn clean(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
        .chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

/// The row that shows the cursor at byte offset `i`. A cursor at the boundary
/// of a soft wrap is drawn at the start of the following row.
fn row_of(rows: &[Row], i: usize) -> usize {
    rows.iter().rposition(|r| r.start <= i).unwrap_or(0)
}

fn wrap_line(line: &str, base: usize, width: usize, rows: &mut Vec<Row>) {
    let mut start = 0;
    let mut used = 0;
    // Where the row can break: just after the last whitespace, and the width up to it.
    let mut last_break: Option<(usize, usize)> = None;
    for (i, g) in line.grapheme_indices(true) {
        let w = g.width();
        let is_space = g.chars().all(char::is_whitespace);
        if used + w > width && used > 0 {
            if is_space {
                // Let the space hang off the end of this row instead of starting the next.
                rows.push(Row { start: base + start, end: base + i + g.len() });
                start = i + g.len();
                used = 0;
                last_break = None;
                continue;
            }
            match last_break {
                Some((at, width_at)) => {
                    rows.push(Row { start: base + start, end: base + at });
                    start = at;
                    used -= width_at;
                }
                None => {
                    rows.push(Row { start: base + start, end: base + i });
                    start = i;
                    used = 0;
                }
            }
            last_break = None;
        }
        used += w;
        if is_space {
            last_break = Some((i + g.len(), used));
        }
    }
    rows.push(Row { start: base + start, end: base + line.len() });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut e = Editor::default();
        e.insert(text);
        e
    }

    fn row_texts(e: &Editor, width: usize) -> Vec<&str> {
        e.rows(width).into_iter().map(|r| e.slice(r)).collect()
    }

    #[test]
    fn wraps_after_whitespace() {
        let e = editor("fix the login redirect");
        assert_eq!(row_texts(&e, 10), ["fix the ", "login ", "redirect"]);
    }

    #[test]
    fn hard_breaks_words_longer_than_the_width() {
        let e = editor("abcdefghij");
        assert_eq!(row_texts(&e, 4), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn keeps_empty_lines() {
        let e = editor("a\n\nb");
        assert_eq!(row_texts(&e, 10), ["a", "", "b"]);
    }

    #[test]
    fn cursor_after_soft_wrap_starts_next_row() {
        let mut e = editor("abcde fg");
        assert_eq!(row_texts(&e, 5), ["abcde ", "fg"]);
        e.line_start();
        for _ in 0..6 {
            e.right();
        }
        assert_eq!(e.cursor_pos(5), (1, 0));
    }

    #[test]
    fn cursor_before_newline_stays_on_its_row() {
        let mut e = editor("ab\ncd");
        e.line_start();
        e.left();
        assert_eq!(e.cursor_pos(10), (0, 2));
    }

    #[test]
    fn vertical_movement_keeps_the_goal_column() {
        let mut e = editor("abcdef\nab\nabcdef");
        // Cursor at the end: row 2, column 6.
        e.move_vertical(20, -1);
        assert_eq!(e.cursor_pos(20), (1, 2));
        e.move_vertical(20, -1);
        assert_eq!(e.cursor_pos(20), (0, 6));
    }

    #[test]
    fn moving_down_onto_a_soft_wrapped_row_stays_on_it() {
        let mut e = editor("ab\nabcde fg");
        e.move_vertical(5, -2);
        e.line_end();
        // Row 0 is "ab", column 2. Row 1 is "abcde " (soft wrapped).
        e.move_vertical(5, 1);
        assert_eq!(e.cursor_pos(5).0, 1);
    }

    #[test]
    fn backspace_removes_whole_graphemes() {
        let mut e = editor("cafe\u{301}");
        e.backspace();
        assert_eq!(e.text(), "caf");
    }

    #[test]
    fn delete_word_left_skips_trailing_punctuation() {
        let mut e = editor("fix the login, ");
        e.delete_word_left();
        assert_eq!(e.text(), "fix the ");
    }

    #[test]
    fn paste_normalizes_line_endings_and_tabs() {
        let e = editor("a\r\nb\tc\u{7}");
        assert_eq!(e.text(), "a\nb    c");
    }

    #[test]
    fn kill_to_line_end_joins_lines_at_the_end() {
        let mut e = editor("ab\ncd");
        e.move_vertical(10, -1);
        e.line_end();
        e.kill_to_line_end();
        assert_eq!(e.text(), "abcd");
    }
}
