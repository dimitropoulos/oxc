//! The crate under test, applied to a whole document.
//!
//! This is the shape a formatter backend will use: walk the tree, ask [`resolve`] once per mapping
//! with that mapping's ancestry, and apply the permutation it hands back.

use oxc_openapi_order::{Options, Scratch, Step, Table, permutation, resolve, resolve_root};

use crate::differential::value::Value;

/// An ancestry step that owns its key, so the walk can hold it while mutating the tree.
#[derive(Debug, Clone)]
pub enum OwnedStep {
    Key(String),
    Index(u32),
}

pub fn borrow(path: &[OwnedStep]) -> Vec<Step<'_>> {
    path.iter()
        .map(|step| match step {
            OwnedStep::Key(key) => Step::Key(key),
            OwnedStep::Index(index) => Step::Index(*index),
        })
        .collect()
}

/// Whether the walk visits a mapping before or after its children.
///
/// Both must give the same answer: a mapping's ancestry does not change when its siblings or its
/// descendants are reordered, so ordering each mapping exactly once is confluent. The test asserts
/// this rather than assuming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    PreOrder,
    PostOrder,
}

/// Order every mapping in `document`.
pub fn order(document: &mut Value, options: &Options<'_>, walk: Order) {
    let mut scratch = Scratch::new();
    let mut path: Vec<OwnedStep> = Vec::new();

    // The root's own table needs the content gate the caller owns. Upstream's spelling is
    // JavaScript truthiness of the `openapi` member, so that is what this harness passes; the
    // formatter backends pass key presence, which the crate docs record as a divergence.
    let has_truthy_openapi = match document {
        Value::Map(entries) => {
            entries.iter().any(|(key, value)| key == "openapi" && value.is_truthy())
        }
        _ => false,
    };

    visit(document, &mut path, options, &mut scratch, walk);

    if let Some(table) = resolve_root(options, has_truthy_openapi)
        && let Value::Map(entries) = document
    {
        apply(entries, table, &mut scratch);
    }
}

fn visit(
    value: &mut Value,
    path: &mut Vec<OwnedStep>,
    options: &Options<'_>,
    scratch: &mut Scratch,
    walk: Order,
) {
    match value {
        Value::Seq(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(OwnedStep::Index(u32::try_from(index).expect("sequence fits in u32")));
                visit(item, path, options, scratch, walk);
                path.pop();
            }
        }
        Value::Map(entries) => {
            if walk == Order::PreOrder
                && let Some(table) = resolve(options, &borrow(path))
            {
                apply(entries, table, scratch);
            }
            for (key, child) in entries.iter_mut() {
                path.push(OwnedStep::Key(key.clone()));
                visit(child, path, options, scratch, walk);
                path.pop();
            }
            if walk == Order::PostOrder
                && let Some(table) = resolve(options, &borrow(path))
            {
                apply(entries, table, scratch);
            }
        }
        _ => {}
    }
}

/// Apply the permutation for `table` to `entries`, if one is needed.
fn apply(entries: &mut Vec<(String, Value)>, table: Table<'_>, scratch: &mut Scratch) {
    let keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
    let Some(permutation) = permutation(table, &keys, scratch) else { return };
    let mut reordered: Vec<(String, Value)> = Vec::with_capacity(entries.len());
    // `permutation` is a permutation of `0..entries.len()`, so every entry moves exactly once.
    let mut taken: Vec<Option<(String, Value)>> = entries.drain(..).map(Some).collect();
    for &index in permutation {
        reordered.push(taken[index as usize].take().expect("a permutation repeats no index"));
    }
    *entries = reordered;
}
