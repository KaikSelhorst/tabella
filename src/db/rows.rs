//! The cells of a result, packed: every value's text in one buffer, where each cell ends, and
//! which cells are NULL. About a third of the memory of one `String` per cell, and cheap to clone.

use std::fmt;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Rows {
    cols: usize,
    len: usize,
    /// Every non-NULL value, back to back.
    text: String,
    /// Where each cell ends in `text`, row by row (a NULL ends where it starts).
    ends: Vec<u32>,
    /// One bit per cell, set for NULL.
    nulls: Vec<u64>,
}

/// One row of a `Rows`.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    rows: &'a Rows,
    at: usize,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The value at row `r`, column `c`; `None` is NULL. Panics when out of range.
    pub fn cell(&self, r: usize, c: usize) -> Option<&str> {
        assert!(
            r < self.len && c < self.cols,
            "cell ({r}, {c}) of {}x{}",
            self.len,
            self.cols
        );
        let i = r * self.cols + c;
        if self.nulls[i / 64] & (1 << (i % 64)) != 0 {
            return None;
        }
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        Some(&self.text[start..self.ends[i] as usize])
    }

    pub fn row(&self, r: usize) -> Row<'_> {
        assert!(r < self.len, "row {r} of {}", self.len);
        Row { rows: self, at: r }
    }

    pub fn get(&self, r: usize) -> Option<Row<'_>> {
        (r < self.len).then_some(Row { rows: self, at: r })
    }

    pub fn first(&self) -> Option<Row<'_>> {
        self.get(0)
    }

    pub fn last(&self) -> Option<Row<'_>> {
        self.len.checked_sub(1).and_then(|r| self.get(r))
    }

    pub fn iter(&self) -> Iter<'_> {
        self.range(..)
    }

    /// The rows in `r` (any range of row numbers).
    pub fn range(&self, r: impl std::ops::RangeBounds<usize>) -> Iter<'_> {
        use std::ops::Bound;
        let start = match r.start_bound() {
            Bound::Included(&s) => s,
            Bound::Excluded(&s) => s + 1,
            Bound::Unbounded => 0,
        };
        let end = match r.end_bound() {
            Bound::Included(&e) => e + 1,
            Bound::Excluded(&e) => e,
            Bound::Unbounded => self.len,
        };
        assert!(
            start <= end && end <= self.len,
            "rows {start}..{end} of {}",
            self.len
        );
        Iter {
            rows: self,
            at: start..end,
        }
    }

    /// The rows `n` at a time (the last group may have fewer).
    pub fn chunks(&self, n: usize) -> impl Iterator<Item = Iter<'_>> {
        (0..self.len)
            .step_by(n.max(1))
            .map(move |s| self.range(s..(s + n).min(self.len)))
    }

    /// Appends a row. The first one sets how many columns every row has. Panics when the text
    /// would pass 4 GiB: for rows built from values already held; a database's go through
    /// `try_push`.
    pub fn push<S: AsRef<str>>(&mut self, row: impl IntoIterator<Item = Option<S>>) {
        assert!(self.try_push(row), "a result's text fits in 4 GiB");
    }

    /// Appends a row, or leaves the rows as they were and returns false when its text would take
    /// them past 4 GiB (cell ends are `u32`): the caller stops there, as at the row limit.
    #[must_use]
    pub fn try_push<S: AsRef<str>>(&mut self, row: impl IntoIterator<Item = Option<S>>) -> bool {
        self.push_within(row, u32::MAX)
    }

    /// `try_push` with the text capped at `limit` bytes (the tests use a small one).
    fn push_within<S: AsRef<str>>(&mut self, row: impl IntoIterator<Item = Option<S>>, limit: u32) -> bool {
        let (before, text, nulls) = (self.ends.len(), self.text.len(), self.nulls.len());
        for v in row {
            let i = self.ends.len();
            if i / 64 >= self.nulls.len() {
                self.nulls.push(0);
            }
            match v {
                Some(s) => self.text.push_str(s.as_ref()),
                None => self.nulls[i / 64] |= 1 << (i % 64),
            }
            let Some(end) = u32::try_from(self.text.len()).ok().filter(|&e| e <= limit) else {
                self.text.truncate(text);
                self.ends.truncate(before);
                self.nulls.truncate(nulls);
                // A NULL bit this row set in a word it shares with the previous row.
                if !before.is_multiple_of(64) {
                    self.nulls[before / 64] &= (1 << (before % 64)) - 1;
                }
                return false;
            };
            self.ends.push(end);
        }
        let width = self.ends.len() - before;
        if self.len == 0 {
            self.cols = width;
        }
        assert_eq!(width, self.cols, "every row has the same columns");
        self.len += 1;
        true
    }

    /// Appends every row of `other` (same columns).
    pub fn extend(&mut self, other: &Rows) {
        for row in other.iter() {
            self.push(row.iter());
        }
    }

    pub fn clear(&mut self) {
        *self = Rows::default();
    }

    /// Gives back what the buffers grew past their contents (once a result is complete).
    pub fn shrink_to_fit(&mut self) {
        self.text.shrink_to_fit();
        self.ends.shrink_to_fit();
        self.nulls.shrink_to_fit();
    }

    /// Keeps the first `n` rows.
    pub fn truncate(&mut self, n: usize) {
        if n >= self.len {
            return;
        }
        let cells = n * self.cols;
        let text = if cells == 0 {
            0
        } else {
            self.ends[cells - 1] as usize
        };
        self.text.truncate(text);
        self.ends.truncate(cells);
        self.nulls.truncate(cells.div_ceil(64));
        if !cells.is_multiple_of(64) {
            self.nulls[cells / 64] &= (1 << (cells % 64)) - 1;
        }
        self.len = n;
        if n == 0 {
            self.cols = 0;
        }
    }

    /// Replaces one value (moves the text after it: for the odd edit, not for loops; see `map`).
    #[cfg(test)]
    pub fn set(&mut self, r: usize, c: usize, v: Option<&str>) {
        let old = self.cell(r, c).map(str::len).unwrap_or(0);
        let i = r * self.cols + c;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        self.text.replace_range(start..start + old, v.unwrap_or(""));
        let new = v.map_or(0, str::len);
        for e in &mut self.ends[i..] {
            *e = u32::try_from(*e as usize + new - old).expect("a result's text fits in 4 GiB");
        }
        match v {
            Some(_) => self.nulls[i / 64] &= !(1 << (i % 64)),
            None => self.nulls[i / 64] |= 1 << (i % 64),
        }
    }

    /// A copy with each value replaced by what `f(row, column, value)` returns.
    pub fn map<'a>(&'a self, mut f: impl FnMut(usize, usize, Option<&'a str>) -> Option<&'a str>) -> Rows {
        let mut out = Rows::default();
        for (r, row) in self.iter().enumerate() {
            out.push(row.iter().enumerate().map(|(c, v)| f(r, c, v)));
        }
        // Keeps the column count of a result with no rows.
        out.cols = self.cols;
        out.shrink_to_fit();
        out
    }

    /// Where the cells live: tells one result's rows from another's (a new result has new ones).
    pub fn addr(&self) -> usize {
        self.ends.as_ptr() as usize
    }

    /// Bytes held, roughly (for the memory tests).
    #[cfg(test)]
    pub fn heap_size(&self) -> usize {
        self.text.capacity() + self.ends.capacity() * 4 + self.nulls.capacity() * 8
    }
}

impl<'a> Row<'a> {
    /// The value in column `c`; `None` is NULL. Panics when out of range.
    pub fn cell(&self, c: usize) -> Option<&'a str> {
        self.rows.cell(self.at, c)
    }

    /// Like a slice's `get`: `None` past the last column, `Some(None)` for NULL.
    pub fn get(&self, c: usize) -> Option<Option<&'a str>> {
        (c < self.rows.cols).then(|| self.cell(c))
    }

    pub fn first(&self) -> Option<Option<&'a str>> {
        self.get(0)
    }

    pub fn iter(&self) -> Cells<'a> {
        Cells {
            row: *self,
            at: 0..self.rows.cols,
        }
    }

    pub fn to_vec(self) -> Vec<Option<String>> {
        self.iter().map(|v| v.map(str::to_owned)).collect()
    }
}

/// Rows in order, see `Rows::iter` and `Rows::range`.
#[derive(Clone)]
pub struct Iter<'a> {
    rows: &'a Rows,
    at: std::ops::Range<usize>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = Row<'a>;

    fn next(&mut self) -> Option<Row<'a>> {
        self.at.next().map(|at| Row { rows: self.rows, at })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.at.size_hint()
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.at.next_back().map(|at| Row { rows: self.rows, at })
    }
}

impl ExactSizeIterator for Iter<'_> {}

/// A row's values in column order, see `Row::iter`.
#[derive(Clone)]
pub struct Cells<'a> {
    row: Row<'a>,
    at: std::ops::Range<usize>,
}

impl<'a> Iterator for Cells<'a> {
    type Item = Option<&'a str>;

    fn next(&mut self) -> Option<Option<&'a str>> {
        self.at.next().map(|c| self.row.cell(c))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.at.size_hint()
    }
}

impl DoubleEndedIterator for Cells<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.at.next_back().map(|c| self.row.cell(c))
    }
}

impl ExactSizeIterator for Cells<'_> {}

impl<'a> IntoIterator for Row<'a> {
    type Item = Option<&'a str>;
    type IntoIter = Cells<'a>;

    fn into_iter(self) -> Cells<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a Rows {
    type Item = Row<'a>;
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// Owned rows, for the few callers that keep the values (catalog lookups and such).
impl IntoIterator for Rows {
    type Item = Vec<Option<String>>;
    type IntoIter = std::vec::IntoIter<Vec<Option<String>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter().map(|r| r.to_vec()).collect::<Vec<_>>().into_iter()
    }
}

impl<S: AsRef<str>> FromIterator<Vec<Option<S>>> for Rows {
    fn from_iter<I: IntoIterator<Item = Vec<Option<S>>>>(iter: I) -> Self {
        let mut out = Rows::default();
        for row in iter {
            out.push(row);
        }
        out.shrink_to_fit();
        out
    }
}

impl From<Vec<Vec<Option<String>>>> for Rows {
    fn from(rows: Vec<Vec<Option<String>>>) -> Self {
        rows.into_iter().collect()
    }
}

impl fmt::Debug for Rows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl fmt::Debug for Row<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Rows {
        [
            vec![Some("1"), None, Some("")],
            vec![Some("two"), Some("zwei"), None],
            vec![None, None, Some("drei")],
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn cells_read_back_as_written() {
        let r = sample();
        assert_eq!((r.len(), r.row(0).iter().len()), (3, 3));
        assert_eq!(r.cell(0, 0), Some("1"));
        assert_eq!(r.cell(0, 1), None);
        assert_eq!(r.cell(0, 2), Some(""), "an empty string is not NULL");
        assert_eq!(r.row(1).to_vec(), [Some("two".into()), Some("zwei".into()), None]);
        assert_eq!(r.last().unwrap().cell(2), Some("drei"));
        assert_eq!(r.range(1..).count(), 2);
        assert!(r.get(3).is_none());
    }

    #[test]
    fn set_moves_the_text_after_it() {
        let mut r = sample();
        r.set(0, 1, Some("one"));
        r.set(1, 0, None);
        r.set(0, 0, Some("longer value"));
        assert_eq!(
            r.row(0).to_vec(),
            [Some("longer value".into()), Some("one".into()), Some("".into())]
        );
        assert_eq!(r.row(1).to_vec(), [None, Some("zwei".into()), None]);
        assert_eq!(r.cell(2, 2), Some("drei"));
    }

    #[test]
    fn truncate_and_map_keep_the_rest_intact() {
        let mut r = sample();
        let upper: Vec<String> = r
            .iter()
            .flat_map(|row| row.iter().map(|v| v.unwrap_or("-").to_uppercase()))
            .collect();
        let m = r.map(|row, c, v| {
            if (row, c) == (1, 1) {
                Some(upper[4].as_str())
            } else {
                v
            }
        });
        assert_eq!(m.cell(1, 1), Some("ZWEI"));
        assert_eq!(m.cell(2, 2), Some("drei"));
        r.truncate(1);
        assert_eq!(r, [vec![Some("1"), None, Some("")]].into_iter().collect());
        r.push([Some("x"), Some("y"), Some("z")]);
        assert_eq!(r.cell(1, 2), Some("z"));
    }

    #[test]
    fn a_full_page_takes_a_fraction_of_one_string_per_cell() {
        let page: Vec<Vec<Option<String>>> = (0..5000)
            .map(|r| {
                (0..20)
                    .map(|c| match c % 3 {
                        0 => Some(format!("{}.{:02}", r * 7 + c, r % 100)),
                        1 => Some(format!("value {r} of column {c}")),
                        _ => (r % 7 != 0).then(|| format!("x{r}")),
                    })
                    .collect()
            })
            .collect();
        // What `Vec<Vec<Option<String>>>` holds: each row's vector of 24-byte slots and each
        // value's own allocation, as malloc rounds them (8 bytes of header, 16-byte steps, 32 at least).
        let alloc = |n: usize| ((n + 8).div_ceil(16) * 16).max(32);
        let strings: usize = page.iter().flatten().flatten().map(|s| alloc(s.len())).sum();
        let one_string_per_cell = 5000 * 24 + 5000 * alloc(20 * 24) + strings;
        let packed = Rows::from(page).heap_size();
        assert!(
            packed * 2 < one_string_per_cell,
            "{packed} bytes packed, {one_string_per_cell} one string per cell"
        );
    }

    #[test]
    fn a_row_past_the_text_limit_is_left_out_whole() {
        let mut r = Rows::default();
        for i in 0..70 {
            assert!(r.push_within([Some("ab"), None], 200), "row {i}");
        }
        // Its NULL lands in the word the last row shares, and its text goes past the limit.
        assert!(!r.push_within([None, Some("x".repeat(100).as_str())], 200));
        assert_eq!((r.len(), r.cell(69, 0), r.cell(69, 1)), (70, Some("ab"), None));
        assert!(r.push_within([Some("cd"), Some("e")], 200));
        assert_eq!((r.cell(70, 0), r.cell(70, 1)), (Some("cd"), Some("e")));
    }

    #[test]
    fn many_rows_cross_the_null_words() {
        let mut r = Rows::default();
        for i in 0..100 {
            r.push([(i % 3 == 0).then(|| i.to_string()), None]);
        }
        assert!((0..100).all(|i| r.cell(i, 0).is_some() == (i % 3 == 0) && r.cell(i, 1).is_none()));
        r.truncate(40);
        r.push([Some("a"), Some("b")]);
        assert_eq!(r.row(40).to_vec(), [Some("a".into()), Some("b".into())]);
        assert_eq!(r.cell(39, 0), Some("39"));
    }
}
