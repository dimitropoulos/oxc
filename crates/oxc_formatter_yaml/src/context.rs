use std::cell::{Cell, RefCell};

use oxc_formatter_core::{FormatContext, SourceText};
use oxc_yaml_parser::ast::Root;

use crate::{
    comments::{Comments, SourceComment},
    options::YamlFormatOptions,
    print::OpenapiState,
};

/// Formatting context for YAML.
pub struct YamlFormatContext<'a> {
    options: YamlFormatOptions,
    source_text: SourceText<'a>,
    comments: Comments<'a>,
    /// Number of enclosing block collections.
    /// Maintained by `write_mapping` / `write_sequence`.
    collection_depth: Cell<u32>,
    /// End offset of the stream's last descendant node;
    /// block scalars compare against it.
    last_descendant_end: u32,
    /// Per-run state for OpenAPI key ordering: the shared `oxc_openapi_order` session (the ancestry
    /// of the mapping being printed and its reused ordering buffers) plus the lazily built
    /// anchor/alias index.
    ///
    /// A `RefCell` rather than [`Cell`] because the state is not `Copy`; the discipline is the same
    /// as `collection_depth`'s (per-run, mutated through `&self` at print sites). No borrow is ever
    /// held across a nested write, which is what keeps the shared buffer re-entrant.
    openapi: RefCell<OpenapiState<'a>>,
    /// Whether the document currently being printed is an OpenAPI document.
    /// Per document, not per stream: a stream may mix OpenAPI and non-OpenAPI documents.
    /// Maintained by `write_document`.
    openapi_document: Cell<bool>,
}

impl<'a> YamlFormatContext<'a> {
    pub fn new(
        options: YamlFormatOptions,
        source_code: &'a str,
        comments: &'a [SourceComment],
        last_descendant_end: u32,
        root: &'a Root<'a>,
    ) -> Self {
        Self {
            options,
            source_text: SourceText::new(source_code),
            comments: Comments::new(comments),
            collection_depth: Cell::new(0),
            last_descendant_end,
            openapi: RefCell::new(OpenapiState::new(root)),
            openapi_document: Cell::new(false),
        }
    }

    pub fn collection_depth(&self) -> &Cell<u32> {
        &self.collection_depth
    }

    pub fn last_descendant_end(&self) -> u32 {
        self.last_descendant_end
    }

    /// Per-run OpenAPI ordering state. The borrow discipline is documented on `OpenapiState` itself,
    /// in `print::openapi` (not linked: the type is private, and rustdoc rejects the link from here).
    pub fn openapi(&self) -> &RefCell<OpenapiState<'a>> {
        &self.openapi
    }

    /// Whether the document being printed is an OpenAPI document (its root mapping has an
    /// `openapi` key). Set by `write_document` before the body is written.
    pub fn openapi_document(&self) -> &Cell<bool> {
        &self.openapi_document
    }

    /// Returns the source text with the arena lifetime (vs the trait's borrow-elided `&str`).
    /// Slices taken via this method carry the `'a` lifetime,
    /// so they don't have to be re-allocated for `text(...)`.
    pub fn source_text(&self) -> SourceText<'a> {
        self.source_text
    }

    /// Returns the comment cursor.
    pub fn comments(&self) -> &Comments<'a> {
        &self.comments
    }
}

impl FormatContext for YamlFormatContext<'_> {
    type Options = YamlFormatOptions;

    fn options(&self) -> &Self::Options {
        &self.options
    }

    fn source_code(&self) -> &str {
        &self.source_text
    }
}
