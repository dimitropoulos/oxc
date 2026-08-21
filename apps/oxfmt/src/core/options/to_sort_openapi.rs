//! `FormatConfig` → `oxc_openapi_order::SortOpenapi`.
//!
//! Shared by the YAML and JSON mappers rather than written twice: the option means the same thing in
//! both backends, and a per-backend copy is exactly where the two would drift.

use oxc_openapi_order::{KeyOrderEntry, PathsOrder, SortOpenapi};

use super::super::oxfmtrc::{
    FormatConfig, PathsOrderConfig, SortOpenapiConfig, SortOpenapiUserConfig,
};

/// Convert `sortOpenapi` into the option type both formatter backends consume.
///
/// Opt-out, like `sortPackageJson`: unset means enabled with defaults, and only an explicit
/// `sortOpenapi: false` disables it. Every sub-option is independently optional, so
/// `{ "components": true }` leaves the other three at their defaults.
///
/// NOTE: Pure field translation, and infallible: an unparsable value is a config-resolution error
/// long before this runs.
pub fn to_sort_openapi(config: &FormatConfig) -> SortOpenapi {
    let sort = config
        .sort_openapi
        .clone()
        .map_or_else(|| Some(SortOpenapiConfig::default()), SortOpenapiUserConfig::into_config);
    let Some(sort) = sort else { return false.into() };

    SortOpenapi {
        enabled: true,
        paths: match sort.paths {
            Some(PathsOrderConfig::Path) => PathsOrder::Path,
            Some(PathsOrderConfig::Tags) => PathsOrder::Tags,
            Some(PathsOrderConfig::Original) | None => PathsOrder::Original,
        },
        components: sort.components.unwrap_or(false),
        properties: sort.properties.unwrap_or(false),
        // A `BTreeMap` in the config, so the entries arrive in a deterministic order. The policy only
        // ever looks a name up, so the order does not affect output. It keeps the translation
        // reproducible, which a hash map's iteration would not.
        key_order: sort
            .key_order
            .map(|tables| {
                tables.into_iter().map(|(key, fields)| KeyOrderEntry { key, fields }).collect()
            })
            .unwrap_or_default(),
    }
}
