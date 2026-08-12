//! OpenAPI-aware key ordering policy.
//!
//! A port of the ordering pass that [openapi-format] runs on its default settings
//! (`openapiSort` with `defaultSort.json`). Only the ORDERING is ported: nothing here
//! renames, deletes, or rewrites a value, and nothing changes a node's kind.
//!
//! This crate is deliberately dependency-free and knows no AST. Callers supply a
//! mapping's ancestry and its key texts; they get back a permutation (or "no
//! permutation"). Both the YAML and JSON formatter backends use it, so the policy is
//! defined exactly once.
//!
//! # Why this is not a transcription
//!
//! Upstream keys its tables by the PARENT key and applies them in three roles from a
//! single pre-order traversal: a table can order a node's own entries (SELF), the
//! entries of each of its children (CHILD), or the entries of each element of an array
//! (ARRAY). The three arms are mutually exclusive within one visit, but a single
//! mapping is reachable from TWO visits — its own, and its parent's — so which order
//! survives depends on traversal order and on whether the traversal descends into the
//! deep copy the CHILD arm makes.
//!
//! Transcribing that is a bug factory. Instead this crate inverts the question and asks
//! it once per mapping: "which table orders THIS mapping's own entries?" The answer is a
//! pure function of the mapping's ancestry, and a mapping is ordered exactly once, so
//! idempotency is structural rather than emergent. See [`resolve`] for the rule.
//!
//! # The key contract
//!
//! [`permutation`] compares the strings it is given. Upstream compares the JavaScript string
//! coercion of the PARSED key, which is neither the source text nor the scalar's canonical
//! spelling: with the YAML library it depends on, `0x10` becomes `"16"`, `null` becomes the
//! empty string, and `2.0` and `2` collide. A formatter has the SOURCE slice instead, and must
//! pass the resolved scalar value with quotes and escapes already removed — otherwise a quoted
//! `"beta"` would sort before a plain `alpha`, because `"` is 0x22.
//!
//! # Deliberate divergences from upstream
//!
//! Upstream reaches its result by rebuilding JS objects, which costs it three behaviours
//! a formatter must not have:
//!
//! - **Integer-like keys are reordered.** ECMAScript object property order
//!   (`OrdinaryOwnPropertyKeys`) puts canonical array-index keys first, in ascending
//!   numeric order, regardless of insertion order. Every mapping upstream touches is a
//!   JS object, so `404, 200, default, 301` is enumerated as `200, 301, 404, default`
//!   before any rule runs — even with sorting disabled. It is a property of the object
//!   representation, not an ordering rule, so we keep source order.
//! - **A sequence under an ordered key becomes a mapping.** Upstream's `prioritySort`
//!   accepts anything `typeof x === "object"`, which includes arrays, then rebuilds it
//!   with `Object.keys(..).reduce(.., {})`. `[["a","b"]]` comes out `[{"0":"a","1":"b"}]`,
//!   for ANY table including the empty one. We never change a node's kind.
//! - **A `__proto__` key is dropped.** Rebuilding with `obj[key] = value` cannot
//!   round-trip it. We preserve every key.
//!
//! Two more follow from asking the question once instead of twice:
//!
//! - **Ties break by source position, not by what a parent's table happened to do.** Upstream's
//!   second write is a STABLE re-sort of the first, so keys that its own table ties keep the order
//!   the parent's table gave them. Reproducing that would mean composing two permutations and
//!   reintroducing the traversal dependence this crate exists to remove. Only observable for keys
//!   differing solely in case, inside a mapping whose parent is in child role.
//! - **Sequence elements are never reordered.** Upstream's `sortComponentsSet` also drives an
//!   `arraySort(node, 'name')` pass that reorders the ELEMENTS of `parameters`-style sequences by
//!   their `name` member. [`Options::components`] deliberately does not: that is value-keyed
//!   sequence reordering rather than key ordering, and upstream itself throws on a non-string
//!   `name`.
//!
//! Nothing else is left out of the ordering pass. Casing, filtering, version conversion, overlays,
//! splitting, `operationId` generation and title renaming are separate upstream passes, none of
//! them on by default, and all of them rename, delete or rewrite content.
//!
//! [openapi-format]: https://github.com/thim81/openapi-format

mod compare;
mod config;
mod paths;
mod permute;
mod rule;
mod session;
mod tables;

pub use config::{KeyOrderEntry, SortOpenapi};
pub use paths::{PathsOrder, TAG_METHOD_ORDER, order_by_path, order_by_tags};
pub use permute::{Scratch, permutation};
pub use rule::{KeyOrder, Options, Step, is_paths_mapping, resolve, resolve_root};
pub use session::{Frame, Ordering, Session};
pub use tables::{ALPHABETICAL, Table};
