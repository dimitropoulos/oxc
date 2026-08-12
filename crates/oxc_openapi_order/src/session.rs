//! Per-run state a formatter backend needs to apply the policy.
//!
//! The ancestry the rule resolves against, plus every buffer the ordering needs, reused for the whole
//! run so a multi-megabyte document allocates a bounded number of times rather than once per mapping.
//! Both backends share this, so a mapping's ordering is set up exactly one way.
//!
//! `permutations` and `blanks` are watermarked stacks: a mapping appends its [`Frame`], reads it back
//! by index while printing, and drops it on the way out. That is what lets nested mappings share one
//! buffer — an inner mapping appends beyond the outer watermark.
//!
//! Everything is fed in one item at a time ([`Session::push_key`], [`Session::push_blank`]) rather
//! than through an iterator, so a backend never has to hold a borrow of this state while calling back
//! into its formatter. That distinction is load-bearing: the state lives behind a `RefCell`, and a
//! long borrow across a nested write would panic.

use crate::{
    Options, Step,
    permute::{Scratch, permutation},
    rule::{resolve, resolve_root},
};

/// One mapping's ordering, as a window into the run's shared stacks.
///
/// Not `Copy`, and [`Session::end`] consumes it, so a frame cannot outlive its mapping.
#[derive(Debug)]
pub struct Frame {
    start: usize,
    len: usize,
}

impl Frame {
    /// How many entries this frame covers.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the frame covers no entries. Never true in practice — a frame exists only for a
    /// mapping that needed reordering, which takes at least two entries.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Per-run ordering state. See the module docs for the buffer and borrow discipline.
#[derive(Debug, Default)]
pub struct Session<'a> {
    /// Ancestry of the mapping being printed, root first, INCLUDING its own step.
    ancestry: Vec<Step<'a>>,
    /// Keys of the mapping currently being resolved. Live only between the [`Session::push_key`]
    /// run and the [`Session::permute`] that consumes them.
    keys: Vec<&'a str>,
    /// Concatenated permutations, innermost frame last.
    permutations: Vec<u32>,
    /// Concatenated blank-line snapshots, innermost frame last, indexed by SOURCE position within
    /// the frame.
    blanks: Vec<bool>,
    scratch: Scratch,
}

impl<'a> Session<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current ancestry depth.
    pub fn depth(&self) -> usize {
        self.ancestry.len()
    }

    /// Append an ancestry step, returning the depth its [`Session::pop_step`] must be given.
    ///
    /// The backends can only offer a thin borrow adapter around this, since each reaches its session
    /// through its own formatter, so the balance INVARIANT lives here rather than being restated in
    /// each of them: a print site that recursed and leaked a step is caught once, on the way out.
    pub fn push_step(&mut self, step: Step<'a>) -> usize {
        self.ancestry.push(step);
        self.ancestry.len()
    }

    /// Remove the step whose [`Session::push_step`] returned `depth`.
    pub fn pop_step(&mut self, depth: usize) {
        debug_assert_eq!(self.ancestry.len(), depth, "a nested write unbalanced the ancestry");
        self.ancestry.pop();
    }

    /// The table that orders the mapping at the current ancestry, or `None` to keep source order.
    ///
    /// An empty ancestry is the document root, which needs the caller's content gate:
    /// `is_openapi_root` says whether the root has an `openapi` key.
    /// `options` is deliberately independent of the session's own lifetime: `resolve` only reads the
    /// ancestry, so a caller's tables need not outlive the document being printed.
    pub fn table<'o>(&self, options: &Options<'o>, is_openapi_root: bool) -> Option<&'o [&'o str]> {
        if self.ancestry.is_empty() {
            return resolve_root(options, is_openapi_root);
        }
        resolve(options, &self.ancestry)
    }

    /// Add the next key of the mapping being resolved, in SOURCE order.
    pub fn push_key(&mut self, key: &'a str) {
        self.keys.push(key);
    }

    /// Consume the pushed keys and open a frame, or answer `None` when they are already ordered.
    ///
    /// The keys are cleared either way. On `Some`, the caller must push exactly [`Frame::len`]
    /// blank-line flags with [`Session::push_blank`] before reading the frame back.
    pub fn permute(&mut self, table: &[&str]) -> Option<Frame> {
        let Self { keys, scratch, permutations, blanks, .. } = self;
        let frame = permutation(table, keys, scratch).map(|order| {
            let start = permutations.len();
            debug_assert_eq!(blanks.len(), start, "a previous frame was not ended");
            permutations.extend_from_slice(order);
            Frame { start, len: order.len() }
        });
        keys.clear();
        frame
    }

    /// Record whether a blank line preceded the next entry, in SOURCE order.
    pub fn push_blank(&mut self, frame: &Frame, blank: bool) {
        debug_assert!(
            (frame.start..frame.start + frame.len).contains(&self.blanks.len()),
            "a blank landed outside its frame"
        );
        self.blanks.push(blank);
    }

    /// The source index of the entry to print at output `position`.
    pub fn source_index(&self, frame: &Frame, position: usize) -> usize {
        debug_assert!(position < frame.len);
        self.permutations[frame.start + position] as usize
    }

    /// Whether a blank line preceded the entry now at `position`, as measured in SOURCE order
    /// before anything moved. A blank line travels with its entry.
    pub fn blank_before(&self, frame: &Frame, position: usize) -> bool {
        debug_assert_eq!(self.blanks.len(), frame.start + frame.len, "blanks were not filled");
        self.blanks[frame.start + self.source_index(frame, position)]
    }

    /// Drop the frame. Must be called on every path out of the mapping that opened it.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "taking the frame by value is the point: it cannot be read after its mapping ends"
    )]
    pub fn end(&mut self, frame: Frame) {
        debug_assert_eq!(self.permutations.len(), frame.start + frame.len);
        self.permutations.truncate(frame.start);
        self.blanks.truncate(frame.start);
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::{Options, Step};

    #[test]
    fn frames_nest_and_unwind() {
        let mut session = Session::new();
        for key in ["responses", "links", "description"] {
            session.push_key(key);
        }
        let outer = session.permute(&["description", "headers", "content", "links"]).unwrap();
        assert_eq!(outer.len(), 3);
        for blank in [false, false, true] {
            session.push_blank(&outer, blank);
        }
        // description(2), links(1), responses(0)
        assert_eq!(session.source_index(&outer, 0), 2);
        assert_eq!(session.source_index(&outer, 1), 1);
        assert_eq!(session.source_index(&outer, 2), 0);
        // The blank preceded `description`, so it travels to its new first position.
        assert!(session.blank_before(&outer, 0));
        assert!(!session.blank_before(&outer, 1));

        // A nested mapping appends beyond the outer watermark and unwinds cleanly.
        for key in ["b", "a"] {
            session.push_key(key);
        }
        let inner = session.permute(&[]).unwrap();
        session.push_blank(&inner, false);
        session.push_blank(&inner, false);
        assert_eq!(session.source_index(&inner, 0), 1);
        // The outer frame is untouched while the inner one is live.
        assert_eq!(session.source_index(&outer, 0), 2);
        session.end(inner);
        assert_eq!(session.source_index(&outer, 2), 0);
        session.end(outer);
        assert_eq!(session.depth(), 0);
    }

    #[test]
    fn already_ordered_opens_no_frame_and_clears_keys() {
        let mut session = Session::new();
        session.push_key("a");
        session.push_key("b");
        assert!(session.permute(&[]).is_none());
        // A second call must not see the previous call's keys.
        session.push_key("b");
        session.push_key("a");
        assert!(session.permute(&[]).is_some());
    }

    #[test]
    fn table_uses_the_ancestry_and_gates_the_root() {
        let options = Options::default();
        let mut session = Session::new();
        // Empty ancestry is the root, and needs the caller's content gate.
        assert!(session.table(&options, false).is_none());
        assert_eq!(session.table(&options, true).map(|table| table[0]), Some("openapi"));

        session.push_step(Step::Key("paths"));
        session.push_step(Step::Key("/p"));
        let depth = session.push_step(Step::Key("get"));
        // Non-root: the gate is irrelevant, the ancestry decides.
        assert_eq!(session.table(&options, false).map(|table| table[0]), Some("operationId"));
        session.pop_step(depth);
        assert_eq!(session.depth(), 2);
    }

    /// The balance check is the reason `push_step` hands back a depth at all, so a backend that
    /// recursed and leaked a step is caught rather than silently resolving against a wrong ancestry.
    #[test]
    #[should_panic(expected = "a nested write unbalanced the ancestry")]
    #[cfg(debug_assertions)]
    fn a_leaked_step_is_caught_on_the_way_out() {
        let mut session = Session::new();
        let depth = session.push_step(Step::Key("paths"));
        session.push_step(Step::Key("/leaked"));
        session.pop_step(depth);
    }

    #[test]
    fn index_steps_saturate_rather_than_wrap() {
        assert_eq!(Step::index(3), Step::Index(3));
        // Wrapping would alias a huge index onto a small one that a guard names.
        assert_eq!(Step::index(usize::MAX), Step::Index(u32::MAX));
    }
}
