//! Task graphs: units of work with dependencies, and the bookkeeping that
//! runs them — which nodes are ready, which are running, which can never run
//! because something they wait for failed. It knows nothing about apps,
//! connections or engines, so any workload (and later any test scenario,
//! see `planning/app_workload_layering.md` §C) can drive it.
//!
//! A node may only depend on nodes before it (`deps[i]` < `i`), so every
//! graph is acyclic by construction and runs in a valid order.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Some dependency has not ended yet.
    Waiting,
    Ready,
    Running,
    Ok,
    Failed,
    /// A dependency failed (or itself could not run): this node never runs.
    DepFailed,
}

/// Run state of one graph.
#[derive(Debug)]
pub struct Tracker {
    states: Vec<State>,
    deps: Vec<Vec<usize>>,
    ready: VecDeque<usize>,
}

impl Tracker {
    /// `deps[i]`: the nodes node `i` waits for, all `< i`.
    pub fn new(deps: Vec<Vec<usize>>) -> Self {
        for (i, d) in deps.iter().enumerate() {
            assert!(d.iter().all(|&j| j < i), "node {i} depends on a later node");
        }
        let mut t = Self {
            states: vec![State::Waiting; deps.len()],
            deps,
            ready: VecDeque::new(),
        };
        for i in 0..t.states.len() {
            t.update(i);
        }
        t
    }

    /// Waiting → Ready or DepFailed once its dependencies have ended.
    fn update(&mut self, i: usize) {
        if self.states[i] != State::Waiting {
            return;
        }
        let deps = &self.deps[i];
        if deps
            .iter()
            .any(|&j| matches!(self.states[j], State::Failed | State::DepFailed))
        {
            self.states[i] = State::DepFailed;
        } else if deps.iter().all(|&j| self.states[j] == State::Ok) {
            self.states[i] = State::Ready;
            self.ready.push_back(i);
        }
    }

    /// The next ready node, now Running (lowest index first).
    pub fn next_ready(&mut self) -> Option<usize> {
        let i = self.ready.pop_front()?;
        self.states[i] = State::Running;
        Some(i)
    }

    /// A running node ended; its dependants become ready or fail.
    pub fn finish(&mut self, i: usize, ok: bool) {
        self.states[i] = if ok { State::Ok } else { State::Failed };
        for k in i + 1..self.states.len() {
            self.update(k);
        }
    }

    /// Nodes running now.
    pub fn running(&self) -> usize {
        self.states.iter().filter(|s| **s == State::Running).count()
    }

    /// Nothing ready and nothing running: whatever has not run never will.
    pub fn stuck_or_done(&self) -> bool {
        self.ready.is_empty() && self.running() == 0
    }

    pub fn state(&self, i: usize) -> State {
        self.states[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// plan → search → 3 fetches → synthesis
    fn search() -> Tracker {
        Tracker::new(vec![
            vec![],
            vec![0],
            vec![1],
            vec![1],
            vec![1],
            vec![2, 3, 4],
        ])
    }

    #[test]
    fn nodes_run_after_their_dependencies() {
        let mut t = search();
        assert_eq!(t.next_ready(), Some(0));
        assert_eq!(t.next_ready(), None);
        t.finish(0, true);
        assert_eq!(t.next_ready(), Some(1));
        t.finish(1, true);
        let fetches: Vec<_> = std::iter::from_fn(|| t.next_ready()).collect();
        assert_eq!(fetches, [2, 3, 4]);
        t.finish(2, true);
        t.finish(4, true);
        assert_eq!(t.next_ready(), None, "synthesis waits for every fetch");
        t.finish(3, true);
        assert_eq!(t.next_ready(), Some(5));
        t.finish(5, true);
        assert!(t.stuck_or_done());
    }

    #[test]
    fn a_failed_dependency_fails_everything_after_it() {
        let mut t = search();
        t.next_ready();
        t.finish(0, true);
        t.next_ready();
        t.finish(1, true);
        for _ in 0..3 {
            t.next_ready();
        }
        t.finish(2, false);
        t.finish(3, true);
        t.finish(4, true);
        assert_eq!(t.state(5), State::DepFailed);
        assert!(t.stuck_or_done());
        // and transitively: a failed plan fails the whole task
        let mut t = search();
        t.next_ready();
        t.finish(0, false);
        assert!((1..6).all(|i| t.state(i) == State::DepFailed));
    }

    #[test]
    fn nodes_without_dependencies_are_all_ready() {
        let mut t = Tracker::new(vec![vec![]; 4]);
        let all: Vec<_> = std::iter::from_fn(|| t.next_ready()).collect();
        assert_eq!(all, [0, 1, 2, 3]);
    }

    #[test]
    #[should_panic(expected = "later node")]
    fn a_dependency_on_a_later_node_is_refused() {
        Tracker::new(vec![vec![1], vec![]]);
    }
}
