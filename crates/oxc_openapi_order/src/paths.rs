//! Ordering the members of the `paths` mapping.
//!
//! Two comparators, both case-sensitive, unlike the key comparator, which lowercases.

use std::cmp::Ordering;

use crate::compare::cmp_code_units;
use crate::permute::{Scratch, checked_len, order_into};

/// How `paths` members are ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathsOrder {
    /// Keep source order (the default).
    #[default]
    Original,
    /// Order by path, segment by segment.
    Path,
    /// Order by the first tag of the first method present.
    Tags,
}

/// The methods consulted, in order, when keying a path item by tag.
///
/// The first method present with a non-empty `tags` sequence supplies the key; a path item
/// with no tagged method keys on the empty string.
pub const TAG_METHOD_ORDER: [&str; 8] =
    ["get", "query", "post", "put", "delete", "patch", "options", "head"];

/// Compare two path templates segment by segment.
///
/// The leading `/` produces an empty first segment, which is skipped: comparison starts at
/// segment 1. A path that runs out of segments sorts first, tested strictly for "no more
/// segments" rather than for emptiness. The truthy version conflates a missing segment with
/// the empty one a trailing slash produces, which makes the comparator non-antisymmetric for
/// `/pets` against `/pets/` (both directions would answer "less", so the result would depend
/// on input order).
///
/// NOTE: because segment 0 is skipped, two paths that differ only in their first segment and
/// have no others compare equal. That is upstream's behaviour, and it cannot arise for
/// well-formed templates, which all begin with `/` and therefore all have an empty segment 0.
fn cmp_path(left: &str, right: &str) -> Ordering {
    let mut left = left.split('/');
    let mut right = right.split('/');
    left.next();
    right.next();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => match cmp_code_units(left, right) {
                Ordering::Equal => {}
                unequal => return unequal,
            },
        }
    }
}

/// Order `paths` by path template. `None` when they are already ordered.
///
/// # Panics
/// Panics if `paths.len()` exceeds `u32::MAX`.
pub fn order_by_path<'s>(paths: &[&str], scratch: &'s mut Scratch) -> Option<&'s [u32]> {
    order_by(paths, scratch, cmp_path)
}

/// Order `paths` by their tag keys, which the caller extracts with [`TAG_METHOD_ORDER`].
/// `None` when they are already ordered.
///
/// `tags` is parallel to the `paths` mapping's entries; use `""` for a path item with no
/// tagged method.
///
/// # Panics
/// Panics if `tags.len()` exceeds `u32::MAX`.
pub fn order_by_tags<'s>(tags: &[&str], scratch: &'s mut Scratch) -> Option<&'s [u32]> {
    order_by(tags, scratch, cmp_code_units)
}

/// Shared driver: rank-free ordering of `items` by `compare`, ties keeping source order.
fn order_by<'s>(
    items: &[&str],
    scratch: &'s mut Scratch,
    compare: impl Fn(&str, &str) -> Ordering,
) -> Option<&'s [u32]> {
    if items.len() < 2 {
        return None;
    }
    let len = checked_len(items.len());
    order_into(scratch.order_buffer(), len, |left, right| {
        compare(items[left as usize], items[right as usize]).then_with(|| left.cmp(&right))
    })
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{PathsOrder, Scratch, TAG_METHOD_ORDER, cmp_path, order_by_path, order_by_tags};

    fn order_paths<'k>(paths: &[&'k str]) -> Vec<&'k str> {
        let mut scratch = Scratch::new();
        match order_by_path(paths, &mut scratch) {
            None => paths.to_vec(),
            Some(order) => order.iter().map(|&i| paths[i as usize]).collect(),
        }
    }

    fn order_tags<'k>(tags: &[&'k str]) -> Vec<&'k str> {
        let mut scratch = Scratch::new();
        match order_by_tags(tags, &mut scratch) {
            None => tags.to_vec(),
            Some(order) => order.iter().map(|&i| tags[i as usize]).collect(),
        }
    }

    #[test]
    fn default_is_original() {
        assert_eq!(PathsOrder::default(), PathsOrder::Original);
    }

    #[test]
    fn shorter_path_sorts_first() {
        assert_eq!(order_paths(&["/a/b", "/a", "/b", "/a/a"]), ["/a", "/a/a", "/a/b", "/b"]);
    }

    #[test]
    fn trailing_slash_is_antisymmetric() {
        // The whole point of the strict "no more segments" test: both input orders must
        // agree, and `/pets` must win.
        assert_eq!(order_paths(&["/pets/", "/pets"]), ["/pets", "/pets/"]);
        assert_eq!(order_paths(&["/pets", "/pets/"]), ["/pets", "/pets/"]);
        assert_eq!(cmp_path("/pets", "/pets/"), Ordering::Less);
        assert_eq!(cmp_path("/pets/", "/pets"), Ordering::Greater);
        assert_eq!(cmp_path("/pets", "/pets"), Ordering::Equal);
    }

    #[test]
    fn paths_are_case_sensitive() {
        // 'B' (0x42) < 'a' (0x61): unlike the key comparator, no lowercasing.
        assert_eq!(order_paths(&["/Beta", "/alpha"]), ["/Beta", "/alpha"]);
        assert_eq!(order_paths(&["/alpha", "/Beta"]), ["/Beta", "/alpha"]);
    }

    #[test]
    fn segment_zero_is_skipped() {
        // Faithful to upstream: with no further segments, the first is never compared.
        assert_eq!(cmp_path("pets", "dogs"), Ordering::Equal);
        // Well-formed templates all have an empty segment 0, so this cannot bite them.
        assert_eq!(cmp_path("/pets", "/dogs"), Ordering::Greater);
    }

    #[test]
    fn already_ordered_returns_no_permutation() {
        let mut scratch = Scratch::new();
        assert_eq!(order_by_path(&["/a", "/b"], &mut scratch), None);
        assert_eq!(order_by_path(&["/a"], &mut scratch), None);
        assert_eq!(order_by_tags(&["a", "b"], &mut scratch), None);
    }

    #[test]
    fn path_templates_with_parameters_compare_as_text() {
        assert_eq!(
            order_paths(&["/pets/{petId}", "/pets", "/pets/{petId}/photos"]),
            ["/pets", "/pets/{petId}", "/pets/{petId}/photos"]
        );
    }

    #[test]
    fn tags_order_and_tie_keeps_source_order() {
        assert_eq!(order_tags(&["z", "a"]), ["a", "z"]);
        // Untagged path items key on "" and therefore sort first.
        assert_eq!(order_tags(&["b", "", "a"]), ["", "a", "b"]);
        // Ties keep source order; the two empties stay in their original sequence.
        let mut scratch = Scratch::new();
        assert_eq!(
            order_by_tags(&["", "a", ""], &mut scratch).map(<[u32]>::to_vec),
            Some(vec![0, 2, 1])
        );
    }

    #[test]
    fn tag_method_order_matches_upstream() {
        assert_eq!(
            TAG_METHOD_ORDER,
            ["get", "query", "post", "put", "delete", "patch", "options", "head"]
        );
    }
}
