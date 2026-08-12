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
    config::SortOpenapi,
    paths::{PathsOrder, order_by_path, order_by_tags},
    permute::{Scratch, permutation},
    rule::{is_paths_mapping, resolve, resolve_root},
    tables::Table,
};

/// How one mapping's entries are ordered.
///
/// Chosen by [`Session::ordering`] and consumed by [`Session::permute`], so a backend never decides
/// this for itself.
#[derive(Debug, Clone, Copy)]
pub enum Ordering<'a> {
    /// Rank the pushed keys against a field-order table.
    Table(Table<'a>),
    /// Compare the pushed keys as PATH TEMPLATES (`sortOpenapi.paths: "path"`).
    Path,
    /// Compare the pushed strings as TAG KEYS (`sortOpenapi.paths: "tags"`).
    ///
    /// The caller pushes each entry's tag key rather than its own key, which is why
    /// [`Session::push_key`] is documented as "the string the mapping is ordered BY".
    Tags,
}

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
    pub fn table<'o>(&self, options: &Options<'o>, is_openapi_root: bool) -> Option<Table<'o>> {
        if self.ancestry.is_empty() {
            return resolve_root(options, is_openapi_root);
        }
        resolve(options, &self.ancestry)
    }

    /// Whether the mapping at the current ancestry is the `paths` mapping, and so is ordered by the
    /// `paths` sub-option rather than by a table.
    pub fn is_paths_mapping(&self) -> bool {
        is_paths_mapping(&self.ancestry)
    }

    /// Add the next string the mapping is ordered BY, in SOURCE order.
    ///
    /// Usually the entry's key text. Under [`Ordering::Tags`] it is the entry's tag key
    /// instead, because that ordering does not look at the keys at all — one buffer serves both,
    /// since a mapping is only ever ordered by one of them.
    pub fn push_key(&mut self, key: &'a str) {
        self.keys.push(key);
    }

    /// How the mapping at the current ancestry is ordered, or `None` to keep source order.
    ///
    /// The one place the `paths` sub-option is weighed against the tables, so both backends cannot
    /// disagree about it. An explicit `paths` order wins for the `paths` mapping: no built-in table
    /// names `paths`, so the two can only meet when a `keyOrder` override names it too, and then the
    /// purpose-built option is the more specific answer.
    ///
    /// An empty ancestry is the document root, which needs the caller's content gate:
    /// `is_openapi_root` says whether the root has an `openapi` key.
    pub fn ordering<'o>(
        &self,
        sort: &'o SortOpenapi,
        is_openapi_root: bool,
    ) -> Option<Ordering<'o>> {
        if self.is_paths_mapping() {
            match sort.paths {
                PathsOrder::Path => return Some(Ordering::Path),
                PathsOrder::Tags => return Some(Ordering::Tags),
                PathsOrder::Original => {}
            }
        }
        self.table(&sort.options(), is_openapi_root).map(Ordering::Table)
    }

    /// Consume the pushed strings and open a frame, or `None` when they are already ordered.
    ///
    /// The buffer is cleared either way. On `Some`, the caller must push exactly [`Frame::len`]
    /// blank-line flags with [`Session::push_blank`] before reading the frame back.
    pub fn permute(&mut self, ordering: Ordering<'_>) -> Option<Frame> {
        match ordering {
            Ordering::Table(table) => {
                self.permute_with(|keys, scratch| permutation(table, keys, scratch))
            }
            Ordering::Path => self.permute_with(order_by_path),
            Ordering::Tags => self.permute_with(order_by_tags),
        }
    }

    /// Shared frame bookkeeping: whichever comparator ran, the frame is opened the same way.
    fn permute_with(
        &mut self,
        order: impl for<'s> FnOnce(&[&str], &'s mut Scratch) -> Option<&'s [u32]>,
    ) -> Option<Frame> {
        let Self { keys, scratch, permutations, blanks, .. } = self;
        let frame = order(keys, scratch).map(|order| {
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
    use super::{Ordering, Session};
    use crate::{KeyOrderEntry, Options, PathsOrder, SortOpenapi, Step, tables::Table};

    #[test]
    fn frames_nest_and_unwind() {
        let mut session = Session::new();
        for key in ["responses", "links", "description"] {
            session.push_key(key);
        }
        let outer = session
            .permute(Ordering::Table(Table::Builtin(&[
                "description",
                "headers",
                "content",
                "links",
            ])))
            .unwrap();
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
        let inner = session.permute(Ordering::Table(Table::Builtin(&[]))).unwrap();
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
        assert!(session.permute(Ordering::Table(Table::Builtin(&[]))).is_none());
        // A second call must not see the previous call's keys.
        session.push_key("b");
        session.push_key("a");
        assert!(session.permute(Ordering::Table(Table::Builtin(&[]))).is_some());
    }

    /// The `paths` sub-option and the tables meet only at the `paths` mapping, and the option wins.
    #[test]
    fn the_paths_option_decides_only_the_paths_mapping() {
        let mut session = Session::new();
        session.push_step(Step::Key("paths"));

        // No built-in table names `paths`, so the default leaves it in source order.
        let default = SortOpenapi::default();
        assert!(matches!(default.paths, PathsOrder::Original));
        assert!(session.ordering(&default, false).is_none());

        for (order, expected) in
            [(PathsOrder::Path, "Path"), (PathsOrder::Tags, "Tags"), (PathsOrder::Original, "none")]
        {
            let sort = SortOpenapi { paths: order, ..SortOpenapi::default() };
            let answer = match session.ordering(&sort, false) {
                Some(Ordering::Path) => "Path",
                Some(Ordering::Tags) => "Tags",
                Some(Ordering::Table(_)) => "Table",
                None => "none",
            };
            assert_eq!(answer, expected, "paths: {order:?}");
        }

        // An explicit order also beats a `keyOrder` override that names `paths`, which is the only
        // way the two can both apply.
        let sort = SortOpenapi {
            paths: PathsOrder::Path,
            key_order: vec![KeyOrderEntry {
                key: "paths".to_string(),
                fields: vec!["/z".to_string()],
            }],
            ..SortOpenapi::default()
        };
        assert!(matches!(session.ordering(&sort, false), Some(Ordering::Path)));
        // With no explicit order, that same override IS the answer.
        let sort = SortOpenapi { paths: PathsOrder::Original, ..sort };
        assert!(matches!(session.ordering(&sort, false), Some(Ordering::Table(_))));

        // A mapping that is not `paths` is untouched by the option.
        session.push_step(Step::Key("/p"));
        session.push_step(Step::Key("get"));
        let sort = SortOpenapi { paths: PathsOrder::Path, ..SortOpenapi::default() };
        assert!(matches!(session.ordering(&sort, false), Some(Ordering::Table(_))));
    }

    #[test]
    fn table_uses_the_ancestry_and_gates_the_root() {
        let options = Options::default();
        let mut session = Session::new();
        // Empty ancestry is the root, and needs the caller's content gate.
        assert!(session.table(&options, false).is_none());
        assert_eq!(session.table(&options, true).map(Table::describe), Some("openapi"));

        session.push_step(Step::Key("paths"));
        session.push_step(Step::Key("/p"));
        let depth = session.push_step(Step::Key("get"));
        // Non-root: the gate is irrelevant, the ancestry decides.
        assert_eq!(session.table(&options, false).map(Table::describe), Some("operationId"));
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
