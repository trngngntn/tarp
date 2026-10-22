use crate::ids::PaneId;

/// A pane's size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSize {
    pub columns: usize,
    pub rows: usize,
}

/// The size of each pane in a `%layout-change` layout, such as `285d,40x5,0,0[40x2,0,0,0,40x2,0,3,1]`.
pub fn pane_sizes(layout: &str) -> Option<Vec<(PaneId, PaneSize)>> {
    let (_checksum, cells) = layout.split_once(',')?;
    let mut cursor = Cursor(cells);
    let mut panes = Vec::new();
    cursor.cell(&mut panes)?;
    cursor.0.is_empty().then_some(panes)
}

struct Cursor<'a>(&'a str);

impl Cursor<'_> {
    fn eat(&mut self, c: char) -> bool {
        match self.0.strip_prefix(c) {
            Some(rest) => {
                self.0 = rest;
                true
            }
            None => false,
        }
    }

    fn expect(&mut self, c: char) -> Option<()> {
        self.eat(c).then_some(())
    }

    fn number(&mut self) -> Option<usize> {
        let end = self.0.find(|c: char| !c.is_ascii_digit()).unwrap_or(self.0.len());
        let number = self.0[..end].parse().ok()?;
        self.0 = &self.0[end..];
        Some(number)
    }

    /// `<W>x<H>,<x>,<y>` followed by `,<pane>`, or by child cells in `{}` or `[]`.
    fn cell(&mut self, panes: &mut Vec<(PaneId, PaneSize)>) -> Option<()> {
        let columns = self.number()?;
        self.expect('x')?;
        let rows = self.number()?;
        self.expect(',')?;
        self.number()?;
        self.expect(',')?;
        self.number()?;
        let close = if self.eat('{') {
            '}'
        } else if self.eat('[') {
            ']'
        } else {
            self.expect(',')?;
            let pane = PaneId(self.number()?.try_into().ok()?);
            panes.push((pane, PaneSize { columns, rows }));
            return Some(());
        };
        loop {
            self.cell(panes)?;
            if self.eat(close) {
                return Some(());
            }
            self.expect(',')?;
        }
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
