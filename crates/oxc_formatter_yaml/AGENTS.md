# Coding agent guides for `crates/oxc_formatter_yaml`

Follow @../oxc_formatter_core/FORMATTER_POLICY.md , this file holds only the YAML-specific rules and translations.

## Overview

Prettier compatible YAML formatter (`oxfmt`'s Tier 1 backend), using the `oxc_formatter_core` APIs.

- Built on `oxc_formatter_core` for the language-agnostic IR + Printer + builders + macros
  - See `crates/oxc_formatter_core/AGENTS.md` for the IR/pipeline details
- Two entry points (see their docs in `src/format.rs`):
  - `format()` for standalone files, `format_to_ir()` for embedded use via the dispatcher (e.g. CSS front matter, JSDoc fenced blocks)

### Parser

Parses with [`oxc-yaml-parser`](https://crates.io/crates/oxc-yaml-parser).
Its AST follows `yaml-unist-parser`'s node naming (the nodes Prettier's printer operates on), with spans designed for layout work (see the parser's `ast` module docs)

One caveat the printer still owns: a block scalar's `span` consumes its trailing line breaks.
The split between the scalar's own output and the inter-item gap is printer policy, kept together in `src/print/block.rs` (see `consumed_trailing_newlines` / `item_gap_anchor`).

### Error semantics

The shared policy applies; YAML specifics:

- `oxc-yaml-parser` is fail-fast (no partial AST), so any syntax error is an `Err`
- Under-indented multi-line flow scalars (prettier#8602) are one such error
  - Prettier 3.9.6 also rejects them since its `yaml@2` upgrade, so the string corruption reported there cannot happen in either implementation

### Line endings

The source is normalized to `\n`-only BEFORE parsing (see `parse_root` for why).
The printer re-emits the configured `end_of_line` at the final stage.

A leading BOM is stripped before parsing and re-emitted by `format()`.

### Comments

Positional cursor (`Comments` in `src/comments.rs`), same approach as graphql/json, yaml-unist-parser's attach algorithm is NOT ported.

Placement is decided at print sites; the rules live as doc comments on the placement helpers, all in `src/comments.rs`.
Stream-tail end comments (`write_end_comments`) are the one document-layer exception, in `src/print/document.rs`.

### OpenAPI key ordering (`sortOpenapi`)

Oxfmt's own extension, not a Prettier option, and on by default, so an OpenAPI document's key order
deliberately differs from Prettier's. Content-gated: nothing is reordered unless the document's root
mapping has an `openapi` key, and a document without one is byte-identical with it on.

- The ordering policy is `oxc_openapi_order` (no AST, no allocator, no dependencies), shared with the
  JSON backend so it is defined once. Its docs carry the rule and the deliberate divergences from
  openapi-format. The per-run state is that crate's `Session`, also shared, so a mapping's ordering
  is set up exactly one way in both backends; only the anchor/alias index is YAML-side.
- The printer side is `src/print/openapi.rs`: the ancestry the policy needs, the refusals, and the
  blank-line snapshot. `write_mapping` iterates through an optional permutation.
- Reordering is refused, leaving source order, by one function (`may_reorder`) whenever it would not
  be safe. Refusing is always safe; the feature degrades to a no-op for that mapping alone. Each
  branch is pinned by a fixture under `tests/fixtures/yaml/openapi/`.
  - The comment refusals are what keep this feature inside the comment placement invariants above:
    reordering entries moves user content across a comment, so the only invariant-clean reordering is
    one with no comment in play. There are two: a comment in the mapping's printing scope, and a
    comment run directly above the mapping when ordering would put a different entry under it. The
    second is invisible to the comment cursor's pending side, because a block mapping's span starts
    at its first entry, so that run is already drained by the time the mapping is reached.
  - Every refusal must survive its own output, or the first pass refuses and the second reorders.
    Three bugs of exactly that shape were found by the fixture harness's idempotency check: refusing
    an explicit `? key` (the printer normalises it away), branching on which quote the source used
    (the printer picks the quote), and counting raw trailing newlines on a block scalar (an empty
    body shifts the count by one).
- Blank lines: measured per entry in source order before permuting, then consulted through the
  permutation, so a blank travels with its entry. A blank before the entry that ends up first is
  dropped, since there is no preceding entry to separate it from, and a blank between a key and its
  first child is already dropped without this feature.
- The `ItemTail` / block-scalar handoff is unchanged. It is the block scalar's position dependence
  (`is_last_descendant`, `ends_with_keep_chomped_block`, both computed from source order) that is
  unsafe under permutation, and `may_reorder` refuses those entries instead.

## Known divergences

Admission reasons and rules: see FORMATTER_POLICY.md "Known divergences". Current divergences:

- anchor/tag order (prettier#19524): source order is preserved, never reordered
- EOF blank lines: the file always ends with exactly one newline, like every other formatter crate
  (`|+` keep-chomped verbatim tails excepted); Prettier YAML alone preserves EOF blank lines verbatim
- keep-chomped tail at a space-only EOF line (no final newline): the line holds no line break, so it adds nothing to the kept tail;
  - Prettier counts it and prints one newline too many, changing the value `"\n"` → `"\n\n"` (prettier#19256 is the nearest issue)
- `# prettier-ignore` range (prettier#13008): suppresses exactly one node, never every following node
- anchor next-line comments (prettier#10518 / #9327): structurally avoided, the positional cursor makes them the next node's leading comments
- blank lines (prettier#15528): one unified rule:
  a blank line right after a node is preserved (normalized to one) if the source had one, never invented, identical for every node kind and context.
  Prettier's matrix (block collections only between documents; mappings only before end comments; unconditional insertion after block scalars) is not ported.
  This also keeps `proseWrap: never` idempotent where Prettier is not (prettier#10776),
  and covers the blank DOUBLED in front of stream-end comments when the last item carries a trailing comment
  (the prettier#9130 shape, resurfaced: one source blank comes out as two)
- folded scalar more-indented lines (prettier#16126): never re-flowed under `proseWrap: always`, their line breaks are literal per YAML folding,
  so Prettier's wrapping at the print width changes the parsed value and breaks idempotency
- "broken but not broken" flow collections: Prettier sometimes emits a newline inside flow brackets while keeping them flat (no trailing comma, `]`/`}` on the content line).
  multiline pairs (spec-example-7-20 / 9-4) and key trailing comments.
  Here a flow collection either fits on one line or breaks normally.
- comment position (spec-example-6-1): a comment stays at its syntactic position; Prettier hoists a comment after `[` onto the `key:` line
- over-indented comments (`key: value` followed by a deeper-indented comment): the value's layout never changes for a comment;
  Prettier breaks the pair onto two lines (`key:\n  value`) — comment indentation alone must not rewrite the preceding node
- trailing comment width (`key: | # ...`): a same-line trailing comment never counts toward the `fits` measurement
  the same treatment Prettier itself gives JS/JSON line comments and yaml flow collections, but not for block scalar header
  The KEY does count: a long key overflowing on `key: |` alone breaks the pair exactly like Prettier

## Verification

Manual checks:

```sh
cargo run -p oxc_formatter_yaml --example yaml_formatter [filename]
# Dump the formatter IR
DUMP_IR=1 cargo run -p oxc_formatter_yaml --example yaml_formatter [filename]
```
