//! An independent transcription of openapi-format's `openapiSort` traversal.
//!
//! This is the oracle. It is written from the reference's JavaScript, arm by arm and guard by
//! guard, and deliberately shares no code with `oxc_openapi_order`, or the differential test would
//! only prove the crate agrees with itself.
//!
//! It reproduces the reference's JavaScript-object artifacts on purpose (index-key bucketing, the
//! sequence-to-mapping rebuild, the dropped `__proto__`), so the comparison can attribute them
//! instead of tripping over them.
//!
//! Fidelity is checked against the real package by `validate_reference.mjs`; see this crate's
//! `tests/differential.rs` for how to run it.

use crate::differential::value::{Value, js_object_order, js_object_order_deep};

/// The reference's options, restricted to the ordering pass.
#[derive(Debug, Clone, Default)]
pub struct RefOptions {
    /// `sortComponentsSet` non-empty (we model it as "every member").
    pub components: bool,
    /// `sortComponentsProps`.
    pub properties: bool,
}

/// One segment of the reference's `this.path`. Array indices are segments too, and they are
/// strings, verified directly against neotraverse.
#[derive(Debug, Clone)]
pub enum Seg {
    Key(String),
    /// The position is deliberately not carried: every guard compares a segment against a name,
    /// and an index segment stringifies to a decimal that matches none of them. What matters is
    /// only that the segment occupies an absolute slot.
    Index,
}

impl Seg {
    /// The segment as a key, or `None` for an index.
    ///
    /// An index segment stringifies to a decimal, which never equals any name the guards test.
    fn as_key(&self) -> Option<&str> {
        match self {
            Seg::Key(key) => Some(key),
            Seg::Index => None,
        }
    }
}

fn seg_at(path: &[Seg], index: usize) -> Option<&str> {
    path.get(index)?.as_key()
}

fn seg_back(path: &[Seg], back: usize) -> Option<&str> {
    seg_at(path, path.len().checked_sub(back + 1)?)
}

/// `defaultSort.json`, transcribed independently of the crate's copy.
fn table(name: &str) -> Option<&'static [&'static str]> {
    const OPERATION: &[&str] =
        &["operationId", "summary", "description", "parameters", "requestBody", "responses"];
    const SCHEMA: &[&str] =
        &["description", "type", "items", "properties", "format", "example", "default"];
    Some(match name {
        "root" => &[
            "openapi",
            "info",
            "servers",
            "paths",
            "components",
            "tags",
            "x-tagGroups",
            "externalDocs",
        ],
        "get" | "query" | "post" | "put" | "patch" | "delete" => OPERATION,
        "parameters" => &["name", "in", "description", "required", "schema"],
        "requestBody" => &["description", "required", "content"],
        "responses" => &["description", "headers", "content", "links"],
        "content" => &[],
        "components" => &["parameters", "schemas", "mediaTypes"],
        "schema" | "schemas" => SCHEMA,
        "properties" => &["description", "type", "items", "format", "example", "default", "enum"],
        _ => return None,
    })
}

/// The table a mapping's parent applies to it, when the parent is in child role.
///
/// `parent` is the parent's own path. This is the reference's first write for a mapping that its
/// parent orders and that then orders itself, the composition the tie-order divergence comes from.
/// The crate's `resolve` deliberately cannot answer this: it reports the one table that wins, so the
/// losing write is invisible there and has to be reconstructed here.
pub fn child_role_table(parent: &[Seg]) -> Option<&'static [&'static str]> {
    let key = seg_back(parent, 0)?;
    if !matches!(key, "responses" | "schemas" | "properties") {
        return None;
    }
    if matches!(seg_back(parent, 1), Some("properties" | "value")) {
        return None;
    }
    if seg_at(parent, 1) == Some("examples") {
        return None;
    }
    table(key)
}

/// `propComparator(priorityArr)`'s verdict for one pair.
pub fn prop_comparator(priority: &[&str], left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if left == right {
        return Ordering::Equal;
    }
    let rank_left = priority.iter().position(|entry| *entry == left);
    let rank_right = priority.iter().position(|entry| *entry == right);
    match (rank_left, rank_right) {
        (Some(l), Some(r)) => l.cmp(&r),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        // `a.toLowerCase() > b.toLowerCase()`: full case mapping, UTF-16 code-unit order.
        // `str::to_lowercase` is used deliberately here; it implements `Final_Sigma`, which is
        // what JavaScript does, and this is test code where the allocation does not matter.
        (None, None) => {
            #[expect(clippy::disallowed_methods, reason = "must match JS toLowerCase exactly")]
            let (l, r) = (left.to_lowercase(), right.to_lowercase());
            l.encode_utf16().cmp(r.encode_utf16())
        }
    }
}

/// `prioritySort(node, priority)` for a mapping: sort the keys, then rebuild the object.
///
/// Rebuilding is where two of the reference's artifacts live: `total[key] = value` cannot create a
/// `__proto__` own property, and the new object re-buckets its index keys.
fn priority_sort_map(entries: &mut Vec<(String, Value)>, priority: &[&str]) {
    entries.retain(|(key, _)| key != "__proto__");
    // `Array.prototype.sort` is stable, so equal keys keep their relative order.
    entries.sort_by(|(left, _), (right, _)| prop_comparator(priority, left, right));
    js_object_order(entries);
}

/// `prioritySort` applied to any value, faithfully including the sequence case.
///
/// The reference guards with `typeof x === "object"`, which is true for arrays, then rebuilds with
/// `Object.keys(..).reduce(.., {})`. A sequence therefore comes back as a mapping keyed by index,
/// for any table including the empty one.
fn priority_sort(value: &mut Value, priority: &[&str]) {
    match value {
        Value::Map(entries) => priority_sort_map(entries, priority),
        Value::Seq(items) => {
            let mut entries: Vec<(String, Value)> =
                items.drain(..).enumerate().map(|(i, item)| (i.to_string(), item)).collect();
            priority_sort_map(&mut entries, priority);
            *value = Value::Map(entries);
        }
        _ => {}
    }
}

/// Run the reference's ordering pass over `document`.
pub fn sort(document: &mut Value, options: &RefOptions) {
    // The input arrives as JavaScript objects, so index-key bucketing is already in force.
    js_object_order_deep(document);

    let mut path = Vec::new();
    visit(document, &mut path, options);

    // `if (jsonObj.openapi)`: plain JavaScript truthiness of the root member.
    if let Value::Map(entries) = document
        && entries.iter().any(|(key, value)| key == "openapi" && value.is_truthy())
        && let Some(root) = table("root")
    {
        let Value::Map(entries) = document else { unreachable!() };
        priority_sort_map(entries, root);
    }
}

/// The pre-order traversal, with the arms applied to each node before descending into it.
fn visit(value: &mut Value, path: &mut Vec<Seg>, options: &RefOptions) {
    if !matches!(value, Value::Map(_) | Value::Seq(_)) {
        return;
    }

    apply_arms(value, path, options);

    // Descend into the node as it now stands: the reference's `update()` replaces the node and
    // its walker re-derives the child list, so a rebuilt node is what gets walked.
    match value {
        Value::Map(entries) => {
            for (key, child) in entries.iter_mut() {
                path.push(Seg::Key(key.clone()));
                visit(child, path, options);
                path.pop();
            }
        }
        Value::Seq(items) => {
            for child in items.iter_mut() {
                path.push(Seg::Index);
                visit(child, path, options);
                path.pop();
            }
        }
        _ => {}
    }
}

/// The arms of the reference's callback, in source order. They are separate `if`s, not a chain, so
/// more than one can fire for the same node and the last write wins.
fn apply_arms(value: &mut Value, path: &[Seg], options: &RefOptions) {
    let own_key = seg_back(path, 0);

    // Components sorting by alphabet, gated on a non-empty `sortComponentsSet`.
    if options.components
        && own_key.is_some()
        && seg_at(path, 0) == Some("components")
        && seg_back(path, 1) == Some("components")
    {
        priority_sort(value, &[]);
    }

    // Sort properties within components by alphabet (`sortComponentsProps`).
    if options.properties
        && own_key == Some("properties")
        && seg_at(path, 0) == Some("components")
        && seg_at(path, 1) == Some("schemas")
    {
        priority_sort(value, &[]);
    }

    // NOTE: the inline-parameters arm (`arraySort(node, 'name')`) and the paths arm
    // (`sortPathsBy !== 'original'`) are not modelled: the crate deliberately does not implement
    // element reordering, and `PathsOrder` is exercised by the crate's own unit tests. Both are
    // off in every configuration this harness runs.

    // Generic sorting.
    let Some(key) = own_key else { return };
    let Some(priority) = table(key) else { return };

    if matches!(value, Value::Seq(_)) {
        // Array arm: each element takes this key's table.
        if seg_back(path, 1) == Some("example")
            && (seg_at(path, 0) == Some("components") || seg_at(path, 3) == Some("requestBody"))
        {
            return;
        }
        let Value::Seq(items) = value else { unreachable!() };
        for item in items.iter_mut() {
            priority_sort(item, priority);
        }
        return;
    }

    let is_trio = matches!(key, "responses" | "schemas" | "properties");
    if is_trio
        && !matches!(seg_back(path, 1), Some("properties" | "value"))
        && seg_at(path, 1) != Some("examples")
    {
        // Child arm: each mapping child takes this key's table. The node's own order is
        // untouched.
        let Value::Map(entries) = value else { return };
        for (_, child) in entries.iter_mut() {
            if matches!(child, Value::Map(_)) {
                priority_sort(child, priority);
            }
        }
        return;
    }

    // Self arm.
    if seg_at(path, 0) == Some("components")
        && seg_at(path, 1) == Some("examples")
        && seg_at(path, 3) == Some("value")
    {
        return;
    }
    priority_sort(value, priority);
}
