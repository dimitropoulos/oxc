use std::path::Path;

use oxc_allocator::Allocator;
use oxc_formatter_json::{JsonFormatOptions, JsonVariant, format};
use oxc_formatter_tests::{FixtureFormatter, OptionSet, build_fixture_snapshot};

mod options;
use options::apply_json_options;

struct JsonHarness;

impl FixtureFormatter for JsonHarness {
    type Options = JsonFormatOptions;

    fn parse_options(json: &OptionSet) -> Self::Options {
        let mut options = JsonFormatOptions::default();
        apply_json_options(&mut options, json);
        options
    }

    fn format(source: &str, _path: &Path, options: &Self::Options) -> String {
        let allocator = Allocator::default();
        format(&allocator, source, options.clone())
            .expect("format should succeed")
            .print()
            .expect("print should succeed")
            .into_code()
    }
}

fn test_file(path: &Path) {
    // `insta::assert_snapshot!` is invoked from this file so the snapshot's
    // `source:` header records this consumer crate, not the shared harness.
    let snap = build_fixture_snapshot::<JsonHarness>(path);
    insta::with_settings!({
        snapshot_path => snap.path,
        prepend_module_to_snapshot => false,
        snapshot_suffix => "",
        omit_expression => true,
    }, {
        insta::assert_snapshot!(snap.name, snap.body);
    });
}

// Include auto-generated test functions from build.rs
include!(concat!(env!("OUT_DIR"), "/generated_tests.rs"));

// ---

/// Format `source` twice, with OpenAPI key ordering on and off.
fn format_both(source: &str, variant: JsonVariant) -> (String, String) {
    let render = |sort_openapi: bool| {
        let allocator = Allocator::default();
        let options = JsonFormatOptions {
            variant,
            sort_openapi: sort_openapi.into(),
            ..JsonFormatOptions::default()
        };
        format(&allocator, source, options)
            .expect("format should succeed")
            .print()
            .expect("print should succeed")
            .into_code()
    };
    (render(true), render(false))
}

/// The feature's containment guarantee: a document whose root is not an object with an `openapi`
/// member formats IDENTICALLY whether the option is on or off.
///
/// Stronger than a fixture, which can only show that the output is stable: this compares the two
/// option settings against each other, so it fails if the gate ever lets one of these through.
#[test]
fn non_openapi_documents_are_untouched() {
    // Every one of these is packed with table names, so any gate leak reorders something.
    let cases = [
        r#"{"swagger":"2.0","paths":{"/p":{"get":{"responses":{},"summary":"s","operationId":"op"}}}}"#,
        r#"{"paths":{"/p":{"get":{"responses":{},"summary":"s","operationId":"op"}}}}"#,
        // `openapi` nested rather than at the root.
        r#"{"config":{"openapi":"3.0.0","paths":{"/p":{"get":{"responses":{},"operationId":"op"}}}}}"#,
        // A root that is not an object at all.
        r#"[{"responses":{},"summary":"s","operationId":"op"}]"#,
        r#""openapi""#,
        "null",
        "{}",
        // `openapi` as a VALUE, not a key.
        r#"{"x":"openapi","paths":{"/p":{"get":{"responses":{},"operationId":"op"}}}}"#,
        // Near-misses on the key name.
        r#"{"openapix":"3.0.0","paths":{"/p":{"get":{"responses":{},"operationId":"op"}}}}"#,
        r#"{"OpenAPI":"3.0.0","paths":{"/p":{"get":{"responses":{},"operationId":"op"}}}}"#,
        // Comments must not perturb the comparison either.
        "{\"paths\":{\"/p\":{\"get\":{\n// c\n\"responses\":{},\"operationId\":\"op\"}}}}",
    ];
    for variant in [JsonVariant::Json, JsonVariant::Jsonc, JsonVariant::Json5] {
        for source in cases {
            let (on, off) = format_both(source, variant);
            assert_eq!(on, off, "{variant:?} changed a non-OpenAPI document:\n{source}");
        }
    }
}

/// The companion to [`non_openapi_documents_are_untouched`]: the option must actually DO something,
/// or that test passes vacuously.
#[test]
fn the_option_reorders_openapi_documents() {
    let source = r#"{"paths":{"/p":{"get":{"responses":{},"summary":"s","operationId":"op"}}},"openapi":"3.0.0"}"#;
    for variant in [JsonVariant::Json, JsonVariant::Jsonc, JsonVariant::Json5] {
        let (on, off) = format_both(source, variant);
        assert_ne!(on, off, "{variant:?} did not reorder an OpenAPI document");
        // Quote-agnostic: `json5` emits keys unquoted. Unwrapped rather than compared as `Option`s,
        // because `None < Some(_)` would let this pass if `openapi` vanished from the output -- in the
        // one test whose job is to stop `non_openapi_documents_are_untouched` passing vacuously.
        let openapi_at = on.find("openapi").expect("`openapi` missing from the output");
        let paths_at = on.find("paths").expect("`paths` missing from the output");
        assert!(openapi_at < paths_at, "root table not applied: {on}");
    }
    // `json-stringify` has its own printer and is deliberately out of scope.
    let (on, off) = format_both(source, JsonVariant::JsonStringify);
    assert_eq!(on, off, "json-stringify must be untouched");
}
