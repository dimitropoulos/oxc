//! OpenAPI-aware key ordering for block mappings.
//!
//! The ordering policy lives in `oxc_openapi_order`, which knows no AST. This module is the printer
//! side: it tracks the ancestry the policy needs, decides when reordering is safe, and captures the
//! blank-line facts that stop being readable from the source once entries move.
//!
//! Everything here is gated on the document's root mapping having an `openapi` key. A document
//! without one is byte-identical with the feature on.

use oxc_openapi_order::{Frame, Ordering, Session, Step, TAG_METHOD_ORDER};
use oxc_yaml_parser::ast::{
    Chomping, Content, Document, FlowSequenceEntry, Mapping, MappingItem, Node, Root,
};

use crate::{
    comments::{Gap, classify_gap, gap_is_trivia_only, gap_upper_bound},
    print::{
        YamlFormatter,
        block::{item_gap_anchor, last_descendant_block_scalar},
        to_span,
    },
};

/// Per-run state for OpenAPI ordering: the shared [`Session`] plus the one fact only YAML needs.
///
/// Every buffer lives in `oxc_openapi_order`, shared with the JSON backend, so a mapping's ordering
/// is set up exactly one way and the watermarked-stack discipline is described in one place. The
/// anchor/alias index is YAML-only, since JSON has neither.
///
/// Borrow discipline: no borrow of this state is held across a call back into the formatter. Every
/// read takes a fresh short-lived borrow, so a nested mapping can always take its own. That is why
/// [`Session`] is fed one item at a time: computing a key's text or a blank-line fact needs the
/// formatter, and holding a borrow across either would panic the moment anything below it consulted
/// [`YamlFormatContext::openapi`].
///
/// [`YamlFormatContext::openapi`]: crate::context::YamlFormatContext::openapi
pub struct OpenapiState<'a> {
    session: Session<'a>,
    /// Start offsets of every anchor and alias in the stream, ascending. Computed once.
    anchor_and_alias_starts: &'a [u32],
}

impl<'a> OpenapiState<'a> {
    pub fn new(anchor_and_alias_starts: &'a [u32]) -> Self {
        Self { session: Session::new(), anchor_and_alias_starts }
    }
}

/// The source index of the item to print at output `position`.
pub fn source_index(frame: &Frame, position: usize, f: &YamlFormatter<'_, '_>) -> usize {
    f.context().openapi().borrow().session.source_index(frame, position)
}

/// Whether a blank line preceded the item now at `position`, as measured in source order before
/// anything moved. A blank line travels with its item.
pub fn blank_before(frame: &Frame, position: usize, f: &YamlFormatter<'_, '_>) -> bool {
    f.context().openapi().borrow().session.blank_before(frame, position)
}

/// Drop the frame. Must be called on every path out of the mapping that opened it.
pub fn finish(frame: Frame, f: &YamlFormatter<'_, '_>) {
    f.context().openapi().borrow_mut().session.end(frame);
}

/// Runs `write` with `step` appended to the ancestry.
///
/// Push and pop live in one function so no print site can leak a step. The balance check itself is
/// `Session`'s, so both backends are held to it identically; this is only the borrow adapter.
pub fn with_step<'a, R>(
    step: Step<'a>,
    f: &mut YamlFormatter<'_, 'a>,
    write: impl FnOnce(&mut YamlFormatter<'_, 'a>) -> R,
) -> R {
    let depth = f.context().openapi().borrow_mut().session.push_step(step);
    let result = write(f);
    f.context().openapi().borrow_mut().session.pop_step(depth);
    result
}

/// The ancestry step for a mapping item's value.
///
/// Prefers the key's resolved text, since that is what the policy's tables are keyed by. Falls back
/// to the raw source slice for a key that is not a readable scalar (an alias, a collection, a block
/// scalar, or a quoted scalar carrying escapes); no table name can equal such a slice, because each
/// of those forms starts with a character no name contains. The one shape this narrows is a key
/// escaped into a table name (`"\u0073chemas"`), which is not recognised.
pub fn value_step<'a>(item: &MappingItem<'a>, f: &YamlFormatter<'_, 'a>) -> Step<'a> {
    let Some(key) = item.key_content() else { return Step::Key("") };
    let source = f.context().source_text();
    if let Some(text) = resolved_key_text(key, f) {
        return Step::Key(text);
    }
    Step::Key(source.text_for(&to_span(key.content.span())))
}

/// A key's resolved scalar text, or `None` when it is not readable as one.
///
/// "Readable" means a plain-or-quoted scalar whose value is already a slice of the source: no
/// allocation, no escape processing.
///
/// Every test here is on a property the printer preserves, which is what makes the refusal survive
/// its own output. Two earlier versions did not, and the fixture harness's idempotency check caught
/// both:
///
/// - refusing an explicit `? key`, which the printer normalises to `key:` whenever the key is inline
///   and comment-free, so the refusal held on the first pass and not the second;
/// - branching on which quote the source used, when the printer picks the quote itself: `'z''z'` is
///   re-emitted as `"z'z"`, so a rule that refused a single-quoted body containing `'` accepted the
///   same key on the next pass.
///
/// Hence one predicate for both quote styles, and a space or line break refuses outright: the
/// printer may re-fold a multi-line key onto one line under `proseWrap`, and the folded form differs
/// from the source only by the space it leaves behind. No table name contains a space, a quote, a
/// backslash or a line break, so the only cost is declining to reorder a mapping that has such a key
/// at all.
fn resolved_key_text<'a>(key: &Node<'a>, f: &YamlFormatter<'_, 'a>) -> Option<&'a str> {
    let text = scalar_text(key, f)?;
    // A key with a space in it is refused, which no table name has anyway.
    (!text.contains(' ')).then_some(text)
}

/// The resolved text of a single-line scalar, without unescaping.
///
/// Refuses anything that would need real scalar resolution: a multi-line scalar, and a quoted one
/// carrying an escape or an embedded quote. Every caller treats `None` as "cannot read this cheaply"
/// and degrades safely, so widening it is never required for correctness.
fn scalar_text<'a>(node: &Node<'a>, f: &YamlFormatter<'_, 'a>) -> Option<&'a str> {
    let source = f.context().source_text();
    let raw = source.text_for(&to_span(node.content.span()));
    if raw.contains('\n') {
        return None;
    }
    match &node.content {
        Content::Plain(_) => Some(raw),
        Content::QuoteSingle(_) | Content::QuoteDouble(_) => {
            let inner = raw.get(1..raw.len().checked_sub(1)?)?;
            (!inner.contains(['\'', '"', '\\'])).then_some(inner)
        }
        _ => None,
    }
}

/// The value of `name` in `node`, if `node` is a mapping with such an entry.
///
/// Reads block and flow mappings alike: `get: {tags: [pet]}` is as valid a path item as the indented
/// spelling, and the `paths` ordering must not depend on which the author used.
fn mapping_entry<'a, 'n>(
    node: &'n Node<'a>,
    name: &str,
    f: &YamlFormatter<'_, 'a>,
) -> Option<&'n Node<'a>> {
    let children = match &node.content {
        Content::Mapping(mapping) => &mapping.children,
        Content::FlowMapping(mapping) => &mapping.children,
        _ => return None,
    };
    children
        .iter()
        .find(|item| item.key_content().is_some_and(|key| resolved_key_text(key, f) == Some(name)))?
        .value_content()
}

/// The first element of a block or flow sequence.
fn first_element<'a, 'n>(node: &'n Node<'a>) -> Option<&'n Node<'a>> {
    match &node.content {
        Content::Sequence(sequence) => sequence.children.first()?.content.as_deref(),
        Content::FlowSequence(sequence) => match sequence.children.first()? {
            FlowSequenceEntry::Item(node) => Some(node),
            // `[a: b]` is a single-pair mapping, not a scalar, so it names no tag.
            FlowSequenceEntry::Pair(_) => None,
        },
        _ => None,
    }
}

/// A path item's tag key, for `sortOpenapi.paths: "tags"`.
///
/// The first method in [`TAG_METHOD_ORDER`] present with a non-empty `tags` sequence supplies its
/// first tag. A path item with no tagged method keys on the empty string, which sorts before every
/// real tag. That is upstream's behaviour, and it is why the key is a `&str` rather than an
/// `Option`: "untagged" is a position in the order, not a missing answer.
///
/// An unreadable tag (a multi-line or escaped scalar) reads as untagged rather than refusing the
/// mapping. Ordering `paths` moves whole path items and cannot corrupt a document however the keys
/// come out, so degrading here costs nothing but a surprising position.
fn tag_key<'a>(item: &MappingItem<'a>, f: &YamlFormatter<'_, 'a>) -> &'a str {
    let Some(path_item) = item.value_content() else { return "" };
    for method in TAG_METHOD_ORDER {
        if let Some(operation) = mapping_entry(path_item, method, f)
            && let Some(tags) = mapping_entry(operation, "tags", f)
            && let Some(first) = first_element(tags)
            && let Some(tag) = scalar_text(first, f)
        {
            return tag;
        }
    }
    ""
}

/// Whether a tag denotes YAML's merge type, whatever handle it was written with.
///
/// `%TAG` can bind any handle to `tag:yaml.org,2002:`, and this parser stores directives
/// uninterpreted, so matching `!!merge` literally misses `!m!merge` under
/// `%TAG !m! tag:yaml.org,2002:`. That matters: two merge keys have order-dependent precedence, so
/// failing to recognise one can change the document's merged value, and formatting must never do
/// that.
///
/// Deliberately wider than real tag resolution: a local `!merge` bound to nothing is refused too,
/// which costs nothing.
fn tag_is_merge(raw: &str) -> bool {
    if raw == "!<tag:yaml.org,2002:merge>" {
        return true;
    }
    let Some(rest) = raw.strip_prefix('!') else { return false };
    rest.rsplit_once('!').map_or(rest, |(_, suffix)| suffix) == "merge"
}

/// Whether `document`'s root mapping has an `openapi` key.
///
/// The content gate for the whole feature. Deliberately key presence, not JavaScript truthiness of
/// the value the way the reference tests it: a document with `openapi:` and no version is still an
/// OpenAPI document, and a formatter should not decide otherwise. `swagger: "2.0"` does not match,
/// because the root table is 3.x-shaped. `oxc_openapi_order`'s crate docs record the divergence.
pub fn document_is_openapi<'a>(document: &Document<'a>, f: &YamlFormatter<'_, 'a>) -> bool {
    let Some(node) = document.body.content.as_deref() else { return false };
    let Content::Mapping(mapping) = &node.content else { return false };
    mapping.children.iter().any(|item| {
        item.key_content().is_some_and(|key| resolved_key_text(key, f) == Some("openapi"))
    })
}

/// Resolve the permutation for `mapping`, or `None` to keep source order.
///
/// The caller must pair a `Some` result with [`finish`].
pub fn begin_mapping<'a>(mapping: &'a Mapping<'a>, f: &YamlFormatter<'_, 'a>) -> Option<Frame> {
    // Cheapest gates first: the option, the content gate, and mappings with nothing to reorder.
    let sort = &f.options().sort_openapi;
    let is_openapi_root = f.context().openapi_document().get();
    if !sort.enabled || !is_openapi_root {
        return None;
    }
    if mapping.children.len() < 2 {
        return None;
    }

    // How this mapping is ordered: a table, or the `paths` sub-option. The policy weighs those
    // against each other so both backends cannot disagree.
    //
    // `is_openapi_root` is the content gate the policy asks for at an empty ancestry, passed
    // rather than hard-coded so the two spell the gate the same way even if it changes.
    let (ordering, anchors) = {
        let state = f.context().openapi().borrow();
        (state.session.ordering(sort, is_openapi_root)?, state.anchor_and_alias_starts)
    };

    if !may_reorder(mapping, anchors, f) {
        return None;
    }

    for item in &mapping.children {
        // `may_reorder` already established every key is readable.
        let key = item.key_content().expect("checked by `may_reorder`");
        let text = match ordering {
            // Ordering by tag does not look at the keys at all: each entry contributes the tag key
            // of its path item instead.
            Ordering::Tags => tag_key(item, f),
            Ordering::Path | Ordering::Table(_) => {
                resolved_key_text(key, f).expect("checked by `may_reorder`")
            }
        };
        f.context().openapi().borrow_mut().session.push_key(text);
    }

    let frame = f.context().openapi().borrow_mut().session.permute(ordering)?;

    // (6) A comment run directly above the mapping, when ordering would put a different entry
    // under it.
    //
    // `may_reorder`'s `peek` cannot see this one. A block mapping's `span.start` IS its first entry,
    // so `write_node` has already drained and printed everything above it before this runs; the
    // comment is not inside the span, it is immediately before it. Positionally it is the first
    // entry's leading comment, so moving a different entry into first place relocates it across user
    // content -- the `FORMATTER_POLICY.md` invariant (lines 70-79) this whole bail-out exists to
    // honour. It is reachable through the cursor's consumed side.
    //
    // Only the first position matters: an entry swap further down leaves the comment describing the
    // same entry it always did. `own_line_column` separates the two cases: a comment trailing the
    // mapping's own key line (`get: # c`) has none, and does not move.
    if source_index(&frame, 0, f) != 0 {
        let leads_first_entry = f.context().comments().last_consumed().is_some_and(|comment| {
            comment.own_line_column.is_some()
                && comment.span.end <= mapping.span.start
                && gap_is_trivia_only(
                    &f.context().source_text(),
                    comment.span.end,
                    mapping.span.start,
                )
        });
        if leads_first_entry {
            finish(frame, f);
            return None;
        }
    }

    // Snapshot the blank lines in source order, before anything moves. `write_item_separator`
    // measures the gap between two adjacent source offsets, which says nothing about two items that
    // are only adjacent after permuting.
    //
    // One borrow per push, never one held across the loop: `blank_line_between` reads the formatter.
    let push = |blank| f.context().openapi().borrow_mut().session.push_blank(&frame, blank);
    push(false); // the first source item has no predecessor
    for pair in mapping.children.windows(2) {
        push(blank_line_between(&pair[0], &pair[1], f));
    }

    Some(frame)
}

/// Whether the source gap between two adjacent items holds a blank line.
///
/// Measured with exactly the endpoints `write_item_separator` uses, so the snapshot and the
/// unpermuted path agree: `item_gap_anchor` because a block scalar's span swallows its trailing line
/// breaks, and `gap_upper_bound` because a blank in front of a leading comment still counts.
fn blank_line_between<'a>(
    prev: &MappingItem<'a>,
    next: &MappingItem<'a>,
    f: &YamlFormatter<'_, 'a>,
) -> bool {
    let block = prev.value_content().and_then(last_descendant_block_scalar);
    let anchor = item_gap_anchor(block, prev.span.end, f);
    let upper_bound = gap_upper_bound(next.span.start, f);
    anchor < upper_bound
        && classify_gap(f.context().source_text().bytes_range(anchor, upper_bound)) == Gap::Blank
}

/// The bail-out. `false` means "keep source order".
///
/// Refusing is always safe: the feature degrades to a no-op for that one mapping, and every other
/// mapping in the document is unaffected. Each branch below is pinned by a fixture under
/// `tests/fixtures/yaml/openapi/`.
///
/// Every branch must survive its own output, or formatting is not idempotent: a condition the
/// printer erases would refuse on the first pass and reorder on the second. Comments, anchors,
/// aliases, merge keys and block-scalar chomping are all reproduced verbatim, so they do; an earlier
/// version of this function also refused an explicit `? key`, which the printer normalises away, and
/// the fixture harness's idempotency check caught it.
///
/// The comment branch is not an optimisation to be removed later. Comments are placed by a
/// positional, monotonic cursor (`crate::comments`), so moving an entry past another entry would
/// emit comments against the wrong nodes or drop them. It is also what keeps this feature inside
/// `FORMATTER_POLICY.md` "Comment placement invariants" (lines 70-79): a comment must never cross
/// user content, and must never cross a line boundary. Reordering entries moves user content across
/// a comment, so the only invariant-clean reordering is one with no comment in play. The reference
/// implementation deletes every comment on its default settings and relocates them on
/// `keepComments`, which is exactly the behaviour those invariants forbid.
fn may_reorder<'a>(
    mapping: &Mapping<'a>,
    anchor_and_alias_starts: &[u32],
    f: &YamlFormatter<'_, 'a>,
) -> bool {
    let source = f.context().source_text();

    // (1) A comment anywhere this mapping's printing can touch.
    //
    // One `peek` decides it. On entry the cursor sits exactly at the mapping's own start: `write_node`
    // has already drained everything ending at or before it, and no comment inside the span can have
    // been drained, because such a comment ends after that point. So the first pending comment is
    // also the first comment in the span. The scope reaches past `mapping.span.end`, which stops at
    // the last item: a block mapping has no closing token, and `flush_container_end_comments` claims
    // deeper-indented comments that follow it. The one comment that can be pending from before the
    // span is a held-back suppression marker, which this catches too, since it starts earlier still.
    let scope_end = trivia_scope_end(&source, mapping.span.end);
    if f.context().comments().peek().is_some_and(|comment| comment.span.start < scope_end) {
        return false;
    }

    // (2) An anchor or alias inside the span. YAML requires an anchor to precede its alias, so
    // reordering can emit a file that does not parse: a correctness bug rather than a cosmetic one.
    // The reference never meets it because its JSON round-trip expands aliases.
    //
    // Byte-scanning for `&` / `*` would be wrong: they are indicators only at token start, and
    // appear constantly inside URLs, globs and prose. Hence the precomputed index.
    if spans_any(anchor_and_alias_starts, mapping.span.start, mapping.span.end) {
        return false;
    }

    for item in &mapping.children {
        // (3) A key that is not readable as scalar text.
        let Some(key) = item.key_content() else { return false };
        let Some(text) = resolved_key_text(key, f) else { return false };

        // (4) A merge key. `<<` interacts with its siblings by override precedence, so moving it
        // changes the merged result.
        //
        // Any key whose text is `<<` is refused, including a quoted one. Strictly, implicit tag
        // resolution applies to plain scalars only, so `"<<"` is the string `<<` and safe to move,
        // but telling them apart would mean trusting that nothing downstream ever changes the
        // quoting, and refusing a key spelled `<<` costs nothing in a real document.
        if text == "<<" {
            return false;
        }
        if let Some(tag) = key.props.tag
            && tag_is_merge(source.text_for(&to_span(tag.span)))
        {
            return false;
        }

        // (5) A block-scalar tail whose output depends on its position.
        //
        // `write_block_scalar` asks `block.span.end >= last_descendant_end` to decide whether it owns
        // its trailing newlines, and `ends_with_keep_chomped_block` asks whether the stream's last
        // descendant is keep-chomped to decide the file's final newline. Both read source order, so
        // neither follows an item to a new position: moving such an item emits a trailing blank line
        // at EOF, or loses the final newline, and re-formatting that output would change it again.
        //
        // This is why the `ItemTail` / block-scalar handoff itself needs no change: it is the
        // position dependence that is unsafe, and refusing removes it. Conservative on purpose: it
        // refuses whenever any entry carries such a scalar, not only when that entry would move.
        if let Some(block) = item.value_content().and_then(last_descendant_block_scalar) {
            if block.span.end >= f.context().last_descendant_end() {
                return false;
            }
            let tail = source.bytes_range(block.content_end, block.span.end);
            // A handful of bytes; not worth a bytecount dependency (as in `print/block.rs`).
            #[expect(clippy::naive_bytecount)]
            let trailing_newlines = tail.iter().filter(|byte| **byte == b'\n').count();
            // Count blank lines, not raw newlines. For a non-empty body `content_end` sits after the
            // last content byte, so the first newline is that line's own ending; for an empty body it
            // already sits past the header's line break, so every newline in the tail is a blank.
            // The printer normalises a blank run to one, so a raw count is not stable under this
            // branch's own output: an empty body with one blank line counted 2 on the first pass and
            // 1 on the second, which refused and then reordered.
            let body_is_empty = block.content_end > 0
                && source.bytes_range(block.content_end - 1, block.content_end) == [b'\n'];
            let blank_lines = trailing_newlines.saturating_sub(usize::from(!body_is_empty));
            if block.chomping == Chomping::Keep || blank_lines >= 1 {
                return false;
            }
        }
    }

    true
}

/// The first offset at or after `from` that is neither whitespace nor part of a comment line.
///
/// Bounds the region whose comments this mapping's printing can claim.
fn trivia_scope_end(source: &str, from: u32) -> u32 {
    let mut offset = from as usize;
    let bytes = source.as_bytes();
    loop {
        // Skip blanks and line breaks.
        while offset < bytes.len() && matches!(bytes[offset], b' ' | b'\t' | b'\n') {
            offset += 1;
        }
        if offset < bytes.len() && bytes[offset] == b'#' {
            while offset < bytes.len() && bytes[offset] != b'\n' {
                offset += 1;
            }
            continue;
        }
        return u32::try_from(offset).unwrap_or(u32::MAX);
    }
}

/// Whether any offset in the ascending slice `starts` lies in `lo..hi`.
fn spans_any(starts: &[u32], lo: u32, hi: u32) -> bool {
    let from = starts.partition_point(|start| *start < lo);
    starts.get(from).is_some_and(|start| *start < hi)
}

/// Start offsets of every anchor and alias in the stream, ascending.
///
/// Computed once per run: asking per mapping would re-walk each subtree at every enclosing level.
/// Because spans nest, "an anchor starts inside this mapping's span" is exactly "an anchor applies
/// somewhere inside this mapping".
pub fn anchor_and_alias_starts(root: &Root<'_>, into: &mut Vec<u32>) {
    for document in &root.children {
        if let Some(node) = document.body.content.as_deref() {
            collect_node(node, into);
        }
    }
    // The walk is depth-first over source-ordered children, but an anchor precedes the content it
    // applies to, so a defensive sort keeps the binary search's precondition explicit.
    into.sort_unstable();
}

fn collect_node(node: &Node<'_>, into: &mut Vec<u32>) {
    if let Some(anchor) = node.props.anchor {
        into.push(anchor.span.start);
    }
    match &node.content {
        Content::Alias(alias) => into.push(alias.span.start),
        Content::Mapping(mapping) => {
            for item in &mapping.children {
                collect_item(item, into);
            }
        }
        Content::FlowMapping(mapping) => {
            for item in &mapping.children {
                collect_item(item, into);
            }
        }
        Content::Sequence(sequence) => {
            for item in &sequence.children {
                if let Some(node) = item.content.as_deref() {
                    collect_node(node, into);
                }
            }
        }
        Content::FlowSequence(sequence) => {
            for entry in &sequence.children {
                match entry {
                    FlowSequenceEntry::Item(node) => collect_node(node, into),
                    FlowSequenceEntry::Pair(item) => collect_item(item, into),
                }
            }
        }
        Content::Plain(_)
        | Content::QuoteSingle(_)
        | Content::QuoteDouble(_)
        | Content::BlockLiteral(_)
        | Content::BlockFolded(_) => {}
    }
}

/// A mapping item contributes both sides: an anchor can sit on the key (`&k key: v`), which a
/// value-only walk would miss.
fn collect_item(item: &MappingItem<'_>, into: &mut Vec<u32>) {
    if let Some(key) = item.key_content() {
        collect_node(key, into);
    }
    if let Some(value) = item.value_content() {
        collect_node(value, into);
    }
}
