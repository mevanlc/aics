/// A cursor over a fixed ring of items, supporting forward and backward traversal
/// with wraparound. Inspired by `LinkedList::Cursor` but without `Option` — the
/// cursor always points at a valid item.
#[derive(Debug, Clone)]
pub struct RingCursor<T> {
    items: Vec<T>,
    index: usize,
}

impl<T> RingCursor<T> {
    pub fn new(items: Vec<T>) -> Self {
        assert!(!items.is_empty(), "RingCursor requires at least one item");
        Self { items, index: 0 }
    }

    pub fn current(&self) -> &T {
        &self.items[self.index]
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn move_next(&mut self) -> &T {
        self.index = (self.index + 1) % self.items.len();
        self.current()
    }

    pub fn move_prev(&mut self) -> &T {
        self.index = (self.index + self.items.len() - 1) % self.items.len();
        self.current()
    }

    pub fn peek_next(&self) -> &T {
        &self.items[(self.index + 1) % self.items.len()]
    }

    pub fn peek_prev(&self) -> &T {
        &self.items[(self.index + self.items.len() - 1) % self.items.len()]
    }
}

impl<T: PartialEq> RingCursor<T> {
    /// Move the cursor to the given item. Returns `true` if found.
    pub fn set(&mut self, item: &T) -> bool {
        if let Some(i) = self.items.iter().position(|x| x == item) {
            self.index = i;
            true
        } else {
            false
        }
    }
}

impl<T: PartialEq> PartialEq<T> for RingCursor<T> {
    fn eq(&self, other: &T) -> bool {
        self.current() == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_next_wraps() {
        let mut c = RingCursor::new(vec!['A', 'B', 'C']);
        assert_eq!(*c.current(), 'A');
        assert_eq!(*c.move_next(), 'B');
        assert_eq!(*c.move_next(), 'C');
        assert_eq!(*c.move_next(), 'A');
    }

    #[test]
    fn move_prev_wraps() {
        let mut c = RingCursor::new(vec!['A', 'B', 'C']);
        assert_eq!(*c.move_prev(), 'C');
        assert_eq!(*c.move_prev(), 'B');
        assert_eq!(*c.move_prev(), 'A');
    }

    #[test]
    fn peek_does_not_move() {
        let c = RingCursor::new(vec!['A', 'B', 'C']);
        assert_eq!(*c.peek_next(), 'B');
        assert_eq!(*c.peek_prev(), 'C');
        assert_eq!(*c.current(), 'A');
    }

    #[test]
    fn set_moves_cursor() {
        let mut c = RingCursor::new(vec!['A', 'B', 'C']);
        assert!(c.set(&'C'));
        assert_eq!(*c.current(), 'C');
        assert_eq!(*c.move_next(), 'A');
    }

    #[test]
    fn partial_eq_compares_current() {
        let mut c = RingCursor::new(vec!['A', 'B', 'C']);
        assert!(c == 'A');
        c.move_next();
        assert!(c == 'B');
        assert!(c != 'A');
    }

    #[test]
    fn single_item() {
        let mut c = RingCursor::new(vec![42]);
        assert_eq!(*c.current(), 42);
        assert_eq!(*c.move_next(), 42);
        assert_eq!(*c.move_prev(), 42);
    }
}
