//! OpenAPI-aware key ordering for JSON objects.
//!
//! The ordering policy and all the per-run buffers live in `oxc_openapi_order`, shared with the YAML
//! backend so ordering is defined exactly once. This module is the JSON printer side: the ancestry the
//! policy needs, and the decision about when reordering is safe.
//!
//! Gated on the root value being an object with an `openapi` member. A document without one is
//! byte-identical with the feature on.
//!
//! Three of YAML's five refusals cannot arise here, since JSON has no anchors, no aliases and no
//! merge keys, and one is JSON-only: a spread property (`...x`), which the lenient parse accepts and
//! whose position changes what the object evaluates to. The comment refusal applies in full: this
//! crate's parser accepts comments in the `json`, `jsonc` and `json5` variants (see
//! `parse::validate_comments_for_variant`; only `json-stringify` rejects them), and `json` is the
//! variant an OpenAPI `.json` file is formatted with.

use oxc_ast::ast::{
    ArrayExpression, Expression, ObjectExpression, ObjectProperty, ObjectPropertyKind, PropertyKey,
};
use oxc_openapi_order::{Ordering, Session, Step, TAG_METHOD_ORDER};

use crate::print::JsonFormatter;

/// Runs `write` with `step` appended to the ancestry.
///
/// Push and pop live in one function so no print site can leak a step. The balance check itself is
/// `Session`'s, so both backends are held to it identically; this is only the borrow adapter.
pub fn with_step<'a, R>(
    step: Step<'a>,
    f: &mut JsonFormatter<'_, 'a>,
    write: impl FnOnce(&mut JsonFormatter<'_, 'a>) -> R,
) -> R {
    let depth = f.context().openapi().borrow_mut().push_step(step);
    let result = write(f);
    f.context().openapi().borrow_mut().pop_step(depth);
    result
}

/// The ancestry step for an object property's value.
///
/// A property whose key is not readable contributes a step that can match no table name, so the
/// ancestry stays the right length and every later absolute index keeps its meaning.
pub fn value_step<'a>(property: &ObjectProperty<'a>, f: &JsonFormatter<'_, 'a>) -> Step<'a> {
    Step::Key(key_text(&property.key, f).unwrap_or("\0"))
}

/// A property key's identity: the value a consumer sees, not the source spelling.
///
/// `None` means the key has no identity this module can use, and the object must not be reordered.
///
/// Unlike YAML, no unescaping is needed, because the parser already cooked it, and `value` is the same
/// `&'a str` for `"openapi"`, `'openapi'` and (in JSON5) a bare `openapi`.
///
/// A numeric key is the delicate one, for two independent reasons.
///
/// It is read rather than refused, because the printer quotes a numeric key it can round-trip:
/// refusing would refuse the first pass and then read `"42"` as an ordinary string key on the second,
/// and formatting would not be idempotent, the same trap the YAML backend fell into three times.
/// Reading it through the printer's own normaliser is what makes the two passes agree.
///
/// But the printed text is only the key's identity when it round-trips. `1.0` prints as `1.0` while
/// naming the property `1`, so `{1.0: x, "1": y}` is one property written twice. Ordering by printed
/// text would rank those two differently, break the tie that preserves their relative order, and
/// silently flip which one wins, so the formatter would change what the document means. A numeric key
/// that does not round-trip has no usable identity, and the object is refused.
///
/// That refusal survives its own output, which is what makes it correct: both key writers quote a
/// numeric key only when `should_quote_numeric_key` holds, and that requires the same round-trip. A
/// key refused here is therefore emitted bare and is still a non-round-tripping numeric literal on
/// the next pass. The keys that do round-trip are exactly those whose printed text equals
/// `String(Number(..))`, so they cannot alias a differently-spelled sibling, and they read the same
/// whether they come back quoted (`json`) or bare (`json5`).
pub fn key_text<'a>(key: &PropertyKey<'a>, f: &JsonFormatter<'_, 'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StringLiteral(lit) => Some(lit.value.as_str()),
        PropertyKey::StaticIdentifier(ident) => Some(ident.name.as_str()),
        PropertyKey::NumericLiteral(lit) => {
            let printed = super::object::normalized_numeric_key(lit, f);
            super::number_string_round_trips(printed).then_some(printed)
        }
        _ => None,
    }
}

/// Whether the root object has an `openapi` member.
///
/// The content gate for the whole feature. Key presence, not the truthiness of the value the way the
/// reference tests it: a document with `"openapi": ""` is still an OpenAPI document, and a formatter
/// should not decide otherwise. `"swagger": "2.0"` does not match, because the root table is
/// 3.x-shaped. `oxc_openapi_order`'s crate docs record the divergence.
///
/// Runs before the context exists, so it cannot use [`key_text`]; it does not need to, because only a
/// string or identifier key can spell `openapi` and neither needs the context to read.
///
/// Deliberately blind to `computed`: `{["openapi"]: ..}` parses, and the printer emits it as a plain
/// `"openapi"`. Testing `computed` here would open the gate on the second pass but not the first, and
/// the whole document's ordering would depend on which pass you were in.
pub fn is_openapi_root(object: &ObjectExpression<'_>) -> bool {
    object.properties.iter().any(|property| {
        let ObjectPropertyKind::ObjectProperty(prop) = property else { return false };
        match &prop.key {
            PropertyKey::StringLiteral(lit) => lit.value == "openapi",
            PropertyKey::StaticIdentifier(ident) => ident.name == "openapi",
            _ => false,
        }
    })
}

/// Resolve the permutation for `object`'s properties, or `None` to keep source order.
///
/// The caller must pair a `Some` result with [`end`], and must fill the frame's blank-line snapshot
/// with [`push_blanks`] before reading any position out of it.
pub fn begin_object<'a>(
    object: &ObjectExpression<'a>,
    f: &JsonFormatter<'_, 'a>,
) -> Option<oxc_openapi_order::Frame> {
    let sort = &f.options().sort_openapi;
    let is_openapi_root = f.context().openapi_document();
    if !sort.enabled || !is_openapi_root {
        return None;
    }
    if object.properties.len() < 2 {
        return None;
    }

    // How this object is ordered: a table, or the `paths` sub-option. The policy weighs those
    // against each other so both backends cannot disagree.
    //
    // `is_openapi_root` is the content gate the policy asks for at an empty ancestry, passed
    // rather than hard-coded so the two spell the gate the same way even if it changes.
    let ordering = {
        let session = f.context().openapi().borrow();
        session.ordering(sort, is_openapi_root)?
    };

    if !may_reorder(object, f) {
        return None;
    }

    // Keys are pushed one at a time so no borrow of the session is held across a call that takes the
    // formatter. `may_reorder` already established every key is readable.
    for property in &object.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = property else {
            unreachable!("checked by `may_reorder`")
        };
        let key = match ordering {
            // Ordering by tag does not look at the keys at all: each property contributes the tag key
            // of its path item instead.
            Ordering::Tags => tag_key(prop, f),
            Ordering::Path | Ordering::Table(_) => {
                key_text(&prop.key, f).expect("checked by `may_reorder`")
            }
        };
        f.context().openapi().borrow_mut().push_key(key);
    }

    f.context().openapi().borrow_mut().permute(ordering)
}

/// A path item's tag key, for `sortOpenapi.paths: "tags"`.
///
/// The JSON twin of the YAML backend's function of the same name, and it must answer the same for the
/// same document: the first method in [`TAG_METHOD_ORDER`] present with a non-empty `tags` array
/// supplies its first tag, and a path item with no tagged method keys on the empty string, which sorts
/// before every real tag.
///
/// An unreadable tag reads as untagged rather than refusing the object. Ordering `paths` moves whole
/// path items and cannot corrupt the document however the keys come out, so degrading costs nothing
/// but a surprising position.
fn tag_key<'a>(property: &ObjectProperty<'a>, f: &JsonFormatter<'_, 'a>) -> &'a str {
    let Some(path_item) = as_object(&property.value) else { return "" };
    for method in TAG_METHOD_ORDER {
        if let Some(operation) = member(path_item, method, f).and_then(as_object)
            && let Some(tags) = member(operation, "tags", f).and_then(as_array)
            && let Some(first) = tags.elements.first()
            && let Some(tag) = first.as_expression().and_then(string_value)
        {
            return tag;
        }
    }
    ""
}

fn as_object<'a, 'e>(expression: &'e Expression<'a>) -> Option<&'e ObjectExpression<'a>> {
    match expression {
        Expression::ObjectExpression(object) => Some(object),
        _ => None,
    }
}

fn as_array<'a, 'e>(expression: &'e Expression<'a>) -> Option<&'e ArrayExpression<'a>> {
    match expression {
        Expression::ArrayExpression(array) => Some(array),
        _ => None,
    }
}

/// The value of `name` in `object`, if it has such a member.
fn member<'a, 'o>(
    object: &'o ObjectExpression<'a>,
    name: &str,
    f: &JsonFormatter<'_, 'a>,
) -> Option<&'o Expression<'a>> {
    object.properties.iter().find_map(|property| {
        let ObjectPropertyKind::ObjectProperty(property) = property else { return None };
        (key_text(&property.key, f) == Some(name)).then_some(&property.value)
    })
}

/// A string literal's value, or `None` for anything else.
fn string_value<'a>(expression: &Expression<'a>) -> Option<&'a str> {
    match expression {
        Expression::StringLiteral(literal) => Some(literal.value.as_str()),
        _ => None,
    }
}

/// The bail-out. `false` means "keep source order".
///
/// Refusing is always safe: the feature degrades to a no-op for that one object, and every other
/// object in the document is unaffected. Each reachable branch is pinned by a fixture under
/// `tests/fixtures/json/openapi/`; the spread and unknowable-key branches cannot be, because such a
/// document does not format at all (`format()` returns `Err`), so no fixture can hold one. They are
/// defence in depth, kept because a lenient parse is a moving target.
///
/// Every branch must survive its own output, or formatting is not idempotent: a condition the printer
/// erases would refuse on the first pass and reorder on the second. Comments are reproduced verbatim
/// and a spread stays a spread, so both do. Three of the YAML backend's refusals originally branched
/// on properties the printer rewrites, which is how that lesson was learned.
fn may_reorder<'a>(object: &ObjectExpression<'a>, f: &JsonFormatter<'_, 'a>) -> bool {
    // (1) A comment anywhere inside the object.
    //
    // Comments are placed by a positional, monotonic cursor, so moving a property past another
    // property would emit them against the wrong nodes. It is also what keeps this feature inside
    // `FORMATTER_POLICY.md` "Comment placement invariants" (lines 70-79): a comment must never cross
    // user content, and reordering properties moves user content across a comment. Do not "optimise"
    // this away.
    //
    // One `peek` decides it, and unlike YAML the object's own braces make the range exact. On entry
    // the cursor sits just past the `{`: `FmtJsonValue` has drained this object's own leading
    // comments, and every comment inside the braces is still pending, because each property's leading
    // comments are drained inside the property loop. The closing `}` bounds the far end, so there is
    // no equivalent of YAML's comment run that the span does not cover.
    if f.context().comments().peek().is_some_and(|comment| comment.span.start < object.span.end) {
        return false;
    }

    for property in &object.properties {
        // (2) A spread property. JSON-only, and not cosmetic: `{...a, b: 1}` and `{b: 1, ...a}`
        // evaluate differently, and a spread has no key to order by in any case.
        let ObjectPropertyKind::ObjectProperty(prop) = property else { return false };

        // (3) A key with no usable identity; see `key_text` for what that means and why.
        //
        // This is about the key's identity, not about `computed`. A computed key holding a
        // literal (`{["summary"]: ..}`) reads perfectly well, and the printer drops the brackets, so
        // it is ordered like any other key. What cannot be ordered is a key whose text we cannot
        // know (`{[x]: ..}`, which the printer later reports, making the whole format an `Err`) or
        // whose text is not the property it names (a numeric key that does not round-trip).
        if key_text(&prop.key, f).is_none() {
            return false;
        }
    }

    true
}

/// Whether a blank line preceded the entry, in source order.
///
/// Measured with the same endpoints and the same newline counter the unpermuted separator uses, so the
/// two paths agree. `count_newlines` is LS/PS-aware, which this crate requires for every variant: the
/// lenient JS parse accepts U+2028 / U+2029 in an inter-token gap even in `json`.
fn blank_line_between(prev_end: u32, next_start: u32, f: &JsonFormatter<'_, '_>) -> bool {
    if next_start <= prev_end {
        return false;
    }
    let between = f.context().source_text().bytes_range(prev_end, next_start);
    crate::separated::blank_line_after_comma(between)
}

/// Push the blank-line snapshot for `object`, in source order, into `frame`.
///
/// Captured before anything moves: the unpermuted separator measures the gap between two adjacent
/// source offsets, which says nothing about two properties that are only adjacent after permuting.
pub fn push_blanks(
    frame: &oxc_openapi_order::Frame,
    spans: &[oxc_span::Span],
    f: &JsonFormatter<'_, '_>,
) {
    // The first source property has no predecessor.
    f.context().openapi().borrow_mut().push_blank(frame, false);
    for pair in spans.windows(2) {
        let blank = blank_line_between(pair[0].end, pair[1].start, f);
        f.context().openapi().borrow_mut().push_blank(frame, blank);
    }
}

/// Drop the frame.
pub fn end(frame: oxc_openapi_order::Frame, f: &JsonFormatter<'_, '_>) {
    f.context().openapi().borrow_mut().end(frame);
}

/// Read the source index for an output position.
pub fn source_index(
    frame: &oxc_openapi_order::Frame,
    position: usize,
    f: &JsonFormatter<'_, '_>,
) -> usize {
    f.context().openapi().borrow().source_index(frame, position)
}

/// Read the blank-line flag for an output position.
pub fn blank_before(
    frame: &oxc_openapi_order::Frame,
    position: usize,
    f: &JsonFormatter<'_, '_>,
) -> bool {
    f.context().openapi().borrow().blank_before(frame, position)
}

/// The `Session` type as the context stores it.
pub type OpenapiState<'a> = std::cell::RefCell<Session<'a>>;
