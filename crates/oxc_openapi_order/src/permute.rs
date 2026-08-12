//! Turning a table plus a mapping's keys into a permutation.

use std::cmp::Ordering;

use crate::compare::cmp_lowercase;
use crate::tables::Table;

/// Sentinel rank for a key the table does not list. Ranked keys always precede unranked
/// ones, so this must compare greater than every real rank.
const UNRANKED: u32 = u32::MAX;

/// Reusable buffers for one format run.
///
/// A document can hold hundreds of thousands of mappings; allocating per mapping would
/// dominate the cost. Both buffers grow to the widest mapping seen and are then reused, so a
/// whole document costs O(1) allocations.
#[derive(Debug, Default)]
pub struct Scratch {
    ranks: Vec<u32>,
    order: Vec<u32>,
}

impl Scratch {
    /// A fresh, empty scratch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The output buffer, for orderings that need no rank pass.
    pub(crate) fn order_buffer(&mut self) -> &mut Vec<u32> {
        &mut self.order
    }
}

/// The number of entries as a `u32`.
///
/// # Panics
/// Panics if `len` exceeds `u32::MAX`.
pub fn checked_len(len: usize) -> u32 {
    u32::try_from(len).expect("a mapping cannot have more than u32::MAX entries")
}

/// Fill `order` with the permutation of `0..len` that `compare` asks for, or leave it alone
/// and answer `None` when `0..len` is already ordered.
///
/// `compare` must be transitive as well as total. The source index as a final tiebreak supplies
/// totality and antisymmetry only; a non-transitive base comparison would satisfy the letter of
/// that and still break both the early-out below and `sort_unstable_by`, which is permitted to
/// panic on an inconsistent comparator.
pub fn order_into(
    order: &mut Vec<u32>,
    len: u32,
    compare: impl Fn(u32, u32) -> Ordering,
) -> Option<&[u32]> {
    // With the source index as the final tiebreak, an adjacent pair compares `Greater` only
    // when it genuinely needs swapping, so this is an exact "is it sorted" test.
    if (1..len).all(|index| compare(index - 1, index) != Ordering::Greater) {
        return None;
    }

    order.clear();
    order.extend(0..len);
    order.sort_unstable_by(|left, right| compare(*left, *right));
    Some(order)
}

/// The permutation that puts `keys` in `table` order, or `None` when `keys` are already
/// ordered and no work is needed.
///
/// The returned slice is a permutation of `0..keys.len()`: element `i` is the index of the
/// key that belongs at output position `i`.
///
/// Ordering: ranked keys first in table order, then unranked keys compared
/// case-insensitively, then ties broken by source position. An empty table ranks nothing,
/// so it orders the whole mapping case-insensitively.
///
/// The already-ordered early-out is what makes `--check` on an already-formatted
/// multi-megabyte document nearly free: it costs one rank per key plus one comparison per
/// adjacent pair, and never reaches the printer.
///
/// # Panics
/// Panics if `keys.len()` exceeds `u32::MAX`.
pub fn permutation<'s>(
    table: Table<'_>,
    keys: &[&str],
    scratch: &'s mut Scratch,
) -> Option<&'s [u32]> {
    if keys.len() < 2 {
        return None;
    }
    let len = checked_len(keys.len());

    // Split the borrow so the comparison can read `ranks` while `order` is written.
    let Scratch { ranks, order } = scratch;

    // Rank each key once, rather than re-scanning the table inside the comparison.
    ranks.clear();
    ranks.extend(keys.iter().map(|key| table.rank(key).unwrap_or(UNRANKED)));
    let ranks = &*ranks;

    order_into(order, len, |left, right| {
        let (l, r) = (left as usize, right as usize);
        ranks[l]
            .cmp(&ranks[r])
            .then_with(|| cmp_lowercase(keys[l], keys[r]))
            .then_with(|| left.cmp(&right))
    })
}

#[cfg(test)]
mod tests {
    use super::{Scratch, Table, permutation};

    /// The reordered keys, or `None` when no permutation was needed.
    fn apply<'k>(table: &[&str], keys: &[&'k str]) -> Option<Vec<&'k str>> {
        let mut scratch = Scratch::new();
        let permutation = permutation(Table::Builtin(table), keys, &mut scratch)?;
        assert_eq!(permutation.len(), keys.len(), "a permutation covers every key");
        let mut seen = vec![false; keys.len()];
        for &index in permutation {
            assert!(!seen[index as usize], "a permutation repeats no index");
            seen[index as usize] = true;
        }
        Some(permutation.iter().map(|&index| keys[index as usize]).collect())
    }

    /// The final order, whether or not a permutation was needed.
    fn order<'k>(table: &[&str], keys: &[&'k str]) -> Vec<&'k str> {
        apply(table, keys).unwrap_or_else(|| keys.to_vec())
    }

    #[test]
    fn already_ordered_returns_no_permutation() {
        assert_eq!(apply(&["a", "b"], &["a", "b"]), None);
        assert_eq!(apply(&[], &["a", "b", "c"]), None);
        assert_eq!(apply(&["b"], &["b", "a", "c"]), None, "ranked first, then alphabetical");
        assert_eq!(apply(&[], &[]), None);
        assert_eq!(apply(&[], &["only"]), None);
    }

    #[test]
    fn ranked_keys_come_first_in_table_order() {
        let table =
            ["operationId", "summary", "description", "parameters", "requestBody", "responses"];
        assert_eq!(
            order(&table, &["zeta", "responses", "Alpha", "operationId", "tags"]),
            ["operationId", "responses", "Alpha", "tags", "zeta"]
        );
    }

    #[test]
    fn empty_table_is_pure_case_insensitive_alphabetical() {
        assert_eq!(
            order(&[], &["text/plain", "application/json", "Application/XML"]),
            ["application/json", "Application/XML", "text/plain"]
        );
    }

    #[test]
    fn unranked_keys_are_case_insensitive_and_source_stable() {
        assert_eq!(order(&[], &["Foo", "bar", "foo"]), ["bar", "Foo", "foo"]);
        assert_eq!(order(&[], &["foo", "Foo"]), ["foo", "Foo"]);
        assert_eq!(order(&[], &["Foo", "foo"]), ["Foo", "foo"]);
        assert_eq!(order(&[], &["TYPE", "Type", "type"]), ["TYPE", "Type", "type"]);
        assert_eq!(order(&[], &["a", "Type", "type", "b"]), ["a", "b", "Type", "type"]);
    }

    #[test]
    fn ranked_order_is_the_table_order_not_alphabetical() {
        assert_eq!(order(&["b", "a"], &["a", "b"]), ["b", "a"]);
    }

    #[test]
    fn keys_absent_from_the_table_are_ignored() {
        assert_eq!(order(&["description", "type"], &["zz", "type"]), ["type", "zz"]);
        assert_eq!(order(&["nope", "gone"], &["zz", "aa"]), ["aa", "zz"]);
    }

    #[test]
    fn integer_like_keys_are_ordered_as_text_never_numerically() {
        // The reference enumerates integer-like keys in ascending numeric order before any
        // rule runs, because they are JS object keys and that is ECMAScript object property
        // order (`OrdinaryOwnPropertyKeys`), not an ordering rule. Here they are ordinary
        // unranked text, so `10` precedes `2`; the reference would answer ["2", "9", "10"].
        assert_eq!(order(&[], &["10", "9", "2"]), ["10", "2", "9"]);
        // A mapping whose keys are all unranked is still ordered when a table applies at
        // all; keeping source order is `resolve` answering `None`, which is tested there
        // (a `responses` mapping's own status codes are never ordered).
        assert_eq!(
            order(&["description", "headers", "content", "links"], &["404", "200", "default"]),
            ["200", "404", "default"]
        );
    }

    #[test]
    fn unicode_cases_match_javascript() {
        assert_eq!(order(&[], &["z", "a", "\u{c4}"]), ["a", "z", "\u{c4}"]);
        assert_eq!(order(&[], &["z", "\u{130}"]), ["\u{130}", "z"]);
        assert_eq!(order(&[], &["k", "\u{212a}"]), ["k", "\u{212a}"], "tie keeps source order");
        assert_eq!(order(&[], &["\u{fffd}", "\u{1f4a9}"]), ["\u{1f4a9}", "\u{fffd}"]);
    }

    #[test]
    fn final_sigma_changes_the_resulting_order() {
        // A per-character lowercase mapping puts "\u{391}\u{3c2}a" before "\u{391}\u{3a3}";
        // JavaScript does not, because "\u{391}\u{3a3}".toLowerCase() ends in the final form
        // U+03C2, making it a prefix of the other key.
        assert_eq!(
            order(&[], &["\u{391}\u{3c2}a", "\u{3c3}", "A", "\u{391}\u{3a3}"]),
            ["A", "\u{391}\u{3a3}", "\u{391}\u{3c2}a", "\u{3c3}"]
        );
    }

    #[test]
    fn sorting_is_idempotent() {
        let table = ["description", "type", "items", "format", "example", "default", "enum"];
        let keys = ["enum", "zz", "Type", "type", "description", "\u{c4}", "example", "aa"];
        let once = order(&table, &keys);
        assert_eq!(order(&table, &once), once);
        assert_eq!(apply(&table, &once), None, "a second pass needs no work");
    }

    #[test]
    fn scratch_is_reusable_across_mappings() {
        let mut scratch = Scratch::new();
        assert_eq!(permutation(Table::Builtin(&[]), &["b", "a"], &mut scratch), Some(&[1, 0][..]));
        assert_eq!(
            permutation(Table::Builtin(&[]), &["c", "b", "a"], &mut scratch),
            Some(&[2, 1, 0][..])
        );
        // A no-op call must not hand back a stale permutation.
        assert_eq!(permutation(Table::Builtin(&[]), &["a", "b"], &mut scratch), None);
    }
}
