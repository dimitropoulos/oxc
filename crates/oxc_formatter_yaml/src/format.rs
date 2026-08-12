use oxc_allocator::{Allocator, ArenaVec};
use oxc_diagnostics::OxcDiagnostic;
use oxc_formatter_core::{
    Buffer, Document, EmbeddedIr, Format, FormatSession, FormatState, Formatted, VecBuffer,
    builders::{hard_line_break, text},
    write,
};
use oxc_yaml_parser::{Parser, ast::Root};

use crate::{
    comments::SourceComment,
    context::YamlFormatContext,
    options::YamlFormatOptions,
    print::{self, YamlFormatter, to_span},
};

/// Parse `source_text` as a YAML stream and build its formatter IR.
///
/// # Errors
/// Returns an [`OxcDiagnostic`] when the parse fails.
/// `oxc-yaml-parser` is fail-fast (no partial AST), so any syntax error bails out.
pub fn format<'a>(
    allocator: &'a Allocator,
    source_text: &str,
    options: YamlFormatOptions,
) -> Result<Formatted<'a, YamlFormatContext<'a>>, OxcDiagnostic> {
    let (has_bom, source_text) = oxc_formatter_core::spec::split_bom(source_text);
    let parsed = parse_root(allocator, source_text)?;
    let root = parsed.root;
    let source = parsed.source;

    let context = parsed.into_context(options);
    let mut state = FormatState::new(context, allocator);
    // Pre-allocate: measured on 6,925 real-world files (kubernetes, vscode, saleor, bootstrap),
    // 0.3x source bytes plus a 1024-element floor for tiny-file spikes avoids reallocation for 99.9% of the corpus.
    let capacity = (source.len() * 3 / 10).max(1024);
    let mut buffer = VecBuffer::with_capacity(capacity, &mut state);

    write!(&mut buffer, FormatYamlRoot { root, has_bom });

    let elements = buffer.into_vec();
    let context = state.into_context();

    let ir = Document::new(elements, Vec::new());

    Ok(Formatted::new(ir, context))
}

/// Parse `source_text` and build the formatter IR for embedding into another
/// formatter's document (dispatcher path, e.g. a fenced block in markdown).
///
/// Unlike [`format()`], this:
/// - allocates from the session's shared arena and `GroupId` space, so the IR lives as long as the parent's document
/// - emits neither a BOM nor the trailing newline
///
/// # Errors
/// Same as [`format()`]: any parse error bails out.
pub fn format_to_ir<'a>(
    session: &FormatSession<'a>,
    source_text: &str,
    options: YamlFormatOptions,
) -> Result<EmbeddedIr<'a>, OxcDiagnostic> {
    let allocator = session.allocator();
    let parsed = parse_root(allocator, source_text)?;
    let root = parsed.root;

    let context = parsed.into_context(options);
    let mut state = FormatState::new_with_session(context, session.clone());
    let mut buffer = VecBuffer::new(&mut state);

    write!(&mut buffer, FormatYamlEmbedded { root });

    // YAML never collects Tailwind classes.
    Ok(EmbeddedIr { ir: buffer.into_vec(), tailwind_classes: Vec::new() })
}

/// Everything [`parse_root`] derives from the source, ready to seed a [`YamlFormatContext`].
struct ParsedYaml<'a> {
    root: &'a Root<'a>,
    source: &'a str,
    comments: &'a [SourceComment],
}

impl<'a> ParsedYaml<'a> {
    fn into_context(self, options: YamlFormatOptions) -> YamlFormatContext<'a> {
        YamlFormatContext::new(
            options,
            self.source,
            self.comments,
            print::last_descendant_end(self.root),
            self.root,
        )
    }
}

/// Parse the source into the yaml-unist-shaped AST and bridge comment trivia,
/// bailing out on any parse error.
///
/// Copies the source into the arena so every slice taken from it carries `'a`.
/// Entries own the BOM strip; this layer assumes BOM-free input (see [`oxc_formatter_core::spec::split_bom`]).
fn parse_root<'a>(
    allocator: &'a Allocator,
    source_text: &str,
) -> Result<ParsedYaml<'a>, OxcDiagnostic> {
    // NOTE: Normalize line endings BEFORE parsing, unlike other `oxc_formatter_xxx`.
    // For YAML formatter, the printer slices verbatim text from the source in many places.
    // YAML is also unusual in that line breaks and whitespace have meaning.
    let source_text = oxc_formatter_core::normalize_newlines(source_text, ['\r']);
    let source: &'a str = allocator.alloc_str(&source_text);

    let root = Parser::new(allocator, source).parse().map_err(|error| {
        OxcDiagnostic::error(format!("Syntax error: {}", error.kind))
            .with_label(to_span(error.span))
    })?;

    let root = allocator.alloc(root);

    let comments: &'a [SourceComment] = ArenaVec::from_iter_in(
        root.comments
            .iter()
            .map(|c| SourceComment { span: to_span(c.span), own_line_column: c.own_line_column }),
        &allocator,
    )
    .into_arena_slice();

    Ok(ParsedYaml { root, source, comments })
}

/// Emits the stream's documents followed by any trailing comments, and the final newline.
struct FormatYamlRoot<'a> {
    root: &'a Root<'a>,
    has_bom: bool,
}

impl<'a> Format<'a, YamlFormatContext<'a>> for FormatYamlRoot<'a> {
    fn fmt(&self, f: &mut YamlFormatter<'_, 'a>) {
        if self.has_bom {
            write!(f, text("\u{feff}"));
        }

        let keep_chomped_tail = print::write_root(self.root, f);

        // POSIX convention: every formatted file ends with a newline.
        //
        // Prettier suppresses this when the stream's last descendant is a keep-chomped (`+`) block scalar,
        // whose verbatim content already carries the trailing newlines.
        // The scalar side of this handoff is `consumed_trailing_newlines` (print/block.rs):
        // a last-descendant block scalar emits NO trailing newlines of its own,
        // deferring the file tail to this write.
        if !keep_chomped_tail {
            write!(f, hard_line_break());
        }
    }
}

/// Emits the stream's documents and trailing comments only;
/// no BOM, no final newline (the parent document owns the surrounding layout).
struct FormatYamlEmbedded<'a> {
    root: &'a Root<'a>,
}

impl<'a> Format<'a, YamlFormatContext<'a>> for FormatYamlEmbedded<'a> {
    fn fmt(&self, f: &mut YamlFormatter<'_, 'a>) {
        print::write_root(self.root, f);
    }
}
