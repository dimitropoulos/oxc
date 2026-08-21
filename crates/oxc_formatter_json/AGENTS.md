# Coding agent guides for `crates/oxc_formatter_json`

Follow @../oxc_formatter_core/FORMATTER_POLICY.md , this file holds only the JSON-specific rules and translations.

## Overview

Prettier compatible JSON/JSONC/JSON5/JSON.stringify formatter (`oxfmt`'s Tier 1 backend), using the `oxc_formatter_core` APIs.

- Built on `oxc_formatter_core` for the language-agnostic IR + Printer + builders + macros
  - See `crates/oxc_formatter_core/AGENTS.md` for the IR/pipeline details
- Two entry points:
  - `format()`: standalone files (returns a printable `Formatted`)
  - `format_to_ir()`: embedded use via the dispatcher (e.g. a fenced block in JSDoc)
- This crate holds only the JSON-specific layer
- Parses with `oxc_parser`, not `serde_json`
  - For Prettier, JSON is not spec compliant JSON
  - They are subsets of JS expression syntax, so the comments, unquoted key, etc... are allowed as input
- A simplified, independent reimplementation that does not share `oxc_formatter`'s code
  - As a result, although Prettier's JSON (especially JSON5) behaves like JS, as a formatter implementation, they should be distinguished and kept from interfering with each other
  - So, just use `oxc_formatter` (`crates/oxc_formatter/`) as the canonical reference
    - For layout / comment / blank-line decisions when the simplified version is unclear or diverges from Prettier
  - The narrow JSON grammar pays off in speed as well
    - It is ~1.4–2.3x faster than routing the same input through `oxc_formatter` (wrapped in `(...)`)
    - Since the JS path carries expression-kind dispatch, parens, and trivia overhead
    - The gap widens for structure-heavy input and narrows for string-heavy input

### `JsonVariant`

- Json
- Jsonc
- Json5
- JsonStringify

All variants share lenient parsing (comments, trailing commas, single quotes, unquoted keys all parse regardless of variant).
What differs is the output formatting.

Parsing always uses `SourceType::default()` (JS); `variant` only gates comment validation (see `parse.rs`), never the lexis.

- So JS lexer rules including line terminators U+2028 / U+2029 apply to every variant's input, not just JSON5
- Consequence: downstream source scans (newline / blank-line detection) must be LS/PS-aware for all variants
  - Strictly LS/PS are line terminators only in JSON5, but a `json` / `jsonc` input can still carry them in inter-token gaps
  - Because the lenient JS parse accepts them (also matching Prettier, which routes every variant through its JS printer)

See the doc comments on `JsonVariant` in `src/options.rs` for the per-variant rules.

### OpenAPI key ordering (`sortOpenapi`)

Oxfmt's own extension, not a Prettier option, and on by default, so an OpenAPI document's key order
deliberately differs from Prettier's. Content-gated: nothing is reordered unless the root is an
object with an `openapi` member, and any other document formats identically with it on or off
(asserted directly, across all variants, by `non_openapi_documents_are_untouched` in
`tests/fixtures/mod.rs`).

- The ordering policy is `oxc_openapi_order` (no AST, no allocator, no dependencies), shared with the
  YAML backend so it is defined once. Its docs carry the rule and the deliberate divergences from
  openapi-format. The per-run state is that crate's `Session`, also shared, so a mapping's ordering
  is set up exactly one way in both backends.
- The printer side is `src/print/openapi.rs`: the ancestry the policy needs, the refusals, and the
  blank-line snapshot. `FmtJsonObject` iterates through an optional permutation, and `write_separated`
  takes it so the separator and blank-line logic stay in one place.
- Reordering is refused, leaving source order, by one function (`may_reorder`) whenever it would not
  be safe. Refusing is always safe; the feature degrades to a no-op for that object alone. Each
  reachable branch is pinned by a fixture under `tests/fixtures/json/openapi/`. The spread and
  unknowable-key branches are defence in depth and cannot have one, because such a document does not
  format at all.
  - The comment refusal is what keeps this feature inside the comment placement invariants: moving a
    property past another property moves user content across a comment. This is not a JSONC-only
    concern. Every variant except `json-stringify` accepts comments, including the `json` that an
    OpenAPI `.json` file is formatted with.
  - Unlike YAML, one `peek` covers a whole object: braces bound it exactly, so there is no need for
    YAML's second check for a comment run directly above a brace-less block mapping.
  - Every refusal must survive its own output, or the first pass refuses and the second reorders.
    That is why a numeric key is read rather than refused: the `json` variant quotes it, so refusing
    would not be idempotent. Its text comes from the printer's own key normaliser, which answers the
    same whether the key comes back quoted (`json`) or bare (`json5`). The keys that normaliser does
    not settle, those whose printed text is not `String(Number(..))`, like `1.0` or `0x10`, are
    refused, because such a key can alias a differently-spelled sibling and reordering the two would
    change which one wins.
- Blank lines: measured per property in source order before permuting, then consulted through the
  permutation, so a blank travels with its property. A blank before the property that ends up first is
  dropped, since there is no preceding property to separate it from. The measurement must use the same
  endpoints and the same counter as the unpermuted separator, or turning the option on would silently
  add or drop blanks; `blank-lines.json` pins both, including a blank sitting before the comma.
- `json-stringify` is deliberately out of scope: it has a separate printer (`stringify.rs`) whose
  input is machine-generated single-line output, never a hand-written spec.
