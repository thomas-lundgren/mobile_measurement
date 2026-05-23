//! Per-flow server port allocation. test-control owns the range; one port per
//! flow, released on teardown.

use std::collections::HashSet;

pub struct PortAllocator {
    base: u16,
    count: u16,
    used: HashSet<u16>,
}

impl PortAllocator {
    pub fn new(base: u16, count: u16) -> Self {
        Self {
            base,
            count,
            used: HashSet::new(),
        }
    }

    /// Allocate the lowest free port in the range, or None if exhausted.
    pub fn allocate(&mut self) -> Option<u16> {
        (self.base..self.base.saturating_add(self.count)).find(|&p| self.used.insert(p))
    }

    pub fn release(&mut self, port: u16) {
        self.used.remove(&port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_distinct_and_releases() {
        let mut a = PortAllocator::new(41000, 3);
        let p1 = a.allocate().unwrap();
        let p2 = a.allocate().unwrap();
        let p3 = a.allocate().unwrap();
        assert_ne!(p1, p2);
        assert_ne!(p2, p3);
        assert_eq!(a.allocate(), None, "range exhausted");
        a.release(p2);
        assert_eq!(a.allocate(), Some(p2), "released port reused");
    }
}
