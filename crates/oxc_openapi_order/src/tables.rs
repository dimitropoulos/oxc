//! The built-in key-order tables, ported verbatim from openapi-format's `defaultSort.json`.

/// An empty table: every key is unranked, so the mapping is ordered purely
/// case-insensitively. This is what upstream's `prioritySort(node, [])` does, and it is
/// also `content`'s real table.
///
/// NOTE: an empty table is NOT the same as "no table". No table means keep source order;
/// an empty table means alphabetical.
pub const ALPHABETICAL: &[&str] = &[];

/// The table name whose entries order the document root.
pub const ROOT: &str = "root";

/// Keys whose table orders the CHILDREN of the mapping, never the mapping's own entries
/// (upstream's `responses`/`schemas`/`properties` arm).
pub const CHILD_ROLE_KEYS: [&str; 3] = ["responses", "schemas", "properties"];

/// Shared by every HTTP method table; upstream lists the same array six times.
const OPERATION: &[&str] =
    &["operationId", "summary", "description", "parameters", "requestBody", "responses"];

/// `schema` and `schemas` share one array upstream. `properties` differs: it drops
/// `properties` and gains `enum`.
const SCHEMA: &[&str] =
    &["description", "type", "items", "properties", "format", "example", "default"];

/// `defaultSort.json`, entry for entry and in the same order.
pub const TABLES: [(&str, &[&str]); 15] = [
    (
        ROOT,
        &[
            "openapi",
            "info",
            "servers",
            "paths",
            "components",
            "tags",
            "x-tagGroups",
            "externalDocs",
        ],
    ),
    ("get", OPERATION),
    ("query", OPERATION),
    ("post", OPERATION),
    ("put", OPERATION),
    ("patch", OPERATION),
    ("delete", OPERATION),
    ("parameters", &["name", "in", "description", "required", "schema"]),
    ("requestBody", &["description", "required", "content"]),
    ("responses", &["description", "headers", "content", "links"]),
    ("content", ALPHABETICAL),
    ("components", &["parameters", "schemas", "mediaTypes"]),
    ("schema", SCHEMA),
    ("schemas", SCHEMA),
    ("properties", &["description", "type", "items", "format", "example", "default", "enum"]),
];

/// The built-in table for `key`, if any.
pub fn builtin(key: &str) -> Option<&'static [&'static str]> {
    // A linear scan over 15 entries beats hashing, and keeps the crate dependency-free
    // (the workspace bans `std::collections::HashMap`).
    TABLES.iter().find(|(name, _)| *name == key).map(|(_, table)| *table)
}

#[cfg(test)]
mod tests {
    use super::{ALPHABETICAL, CHILD_ROLE_KEYS, TABLES, builtin};

    #[test]
    fn every_table_name_is_unique() {
        for (i, (name, _)) in TABLES.iter().enumerate() {
            assert!(
                !TABLES[..i].iter().any(|(other, _)| other == name),
                "duplicate table name {name}"
            );
        }
    }

    #[test]
    fn child_role_keys_all_have_tables() {
        for key in CHILD_ROLE_KEYS {
            assert!(builtin(key).is_some(), "{key} must have a table");
        }
    }

    #[test]
    fn tables_match_default_sort_json() {
        // Spot-pin the shapes that the ordering rule branches on.
        assert_eq!(builtin("content"), Some(ALPHABETICAL));
        assert_eq!(builtin("schema"), builtin("schemas"));
        assert_ne!(builtin("properties"), builtin("schemas"));
        assert_eq!(builtin("get"), builtin("query"));
        assert_eq!(builtin("requestBody"), Some(&["description", "required", "content"][..]));
        assert_eq!(builtin("nope"), None);
        // `properties` drops `properties` and gains `enum`.
        let properties = builtin("properties").unwrap();
        assert!(!properties.contains(&"properties"));
        assert!(properties.contains(&"enum"));
    }
}
