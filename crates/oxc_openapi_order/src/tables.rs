//! The built-in key-order tables, ported verbatim from openapi-format's `defaultSort.json`.

/// An empty table: every key is unranked, so the mapping is ordered purely
/// case-insensitively. This is what upstream's `prioritySort(node, [])` does, and it is
/// also `content`'s real table.
///
/// NOTE: an empty table is not the same as "no table". No table means keep source order;
/// an empty table means alphabetical.
pub const ALPHABETICAL: &[&str] = &[];

/// The table name whose entries order the document root.
pub const ROOT: &str = "root";

/// Keys whose table orders the children of the mapping, never the mapping's own entries
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

/// A field-order table: the ranked keys for one mapping.
///
/// Two representations, because the built-ins are `&'static [&'static str]` while a user's `keyOrder`
/// arrives as owned strings from a config file. Copying the latter into the former's shape would cost
/// an allocation per format run to answer a question that needs none: a table is only ever asked for
/// a key's rank.
///
/// An empty table of either kind means "order alphabetically", which is not the same as having no
/// table at all ("keep source order"). That is what makes `keyOrder: { content: [] }` meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table<'a> {
    /// A table of string slices: the built-ins, ported from `defaultSort.json`.
    Builtin(&'a [&'a str]),
    /// A `keyOrder` override.
    User(&'a [String]),
}

impl Table<'_> {
    /// The position of `key`, or `None` when this table does not name it.
    ///
    /// Case-sensitive and exact: a table names a key or it does not. Case-insensitivity belongs to
    /// the comparison of unranked keys, which is a different question.
    ///
    /// # Panics
    /// Panics if the table has more than `u32::MAX` entries, which would make a real rank collide
    /// with the "unranked" sentinel.
    pub fn rank(&self, key: &str) -> Option<u32> {
        let position = match self {
            Self::Builtin(table) => table.iter().position(|entry| *entry == key),
            Self::User(table) => table.iter().position(|entry| entry == key),
        };
        position.map(|index| {
            u32::try_from(index).expect("a key-order table cannot have u32::MAX entries")
        })
    }
}

#[cfg(test)]
impl<'a> Table<'a> {
    /// Which table this is, for tests that need to tell them apart.
    ///
    /// Test-only on purpose: nothing in the ordering needs to read a table's contents, only to rank
    /// against them, and a public accessor would invite code that does.
    pub(crate) fn describe(self) -> &'a str {
        match self {
            Self::Builtin(table) => table.first().copied(),
            Self::User(table) => table.first().map(String::as_str),
        }
        .unwrap_or("<alphabetical>")
    }
}

/// The built-in table for `key`, if any.
pub fn builtin(key: &str) -> Option<&'static [&'static str]> {
    // A linear scan over 15 entries beats hashing, and keeps the crate dependency-free
    // (the workspace bans `std::collections::HashMap`).
    TABLES.iter().find(|(name, _)| *name == key).map(|(_, table)| *table)
}

#[cfg(test)]
mod tests {
    use super::{ALPHABETICAL, CHILD_ROLE_KEYS, TABLES, Table, builtin};

    /// Ranking is exact and case-sensitive, matching upstream's `priorityArr.indexOf(key)`.
    ///
    /// Asserted over both representations with the same table: a user's `keyOrder` has to rank
    /// identically to a built-in, or the same document would order differently depending on where
    /// its table came from.
    #[test]
    fn rank_is_exact_and_case_sensitive_in_both_representations() {
        let builtin = Table::Builtin(&["name", "in", "schema"]);
        let owned = vec!["name".to_string(), "in".to_string(), "schema".to_string()];
        let user = Table::User(&owned);
        for table in [builtin, user] {
            assert_eq!(table.rank("name"), Some(0));
            assert_eq!(table.rank("schema"), Some(2));
            assert_eq!(table.rank("Name"), None, "table lookup is case-sensitive");
            assert_eq!(table.rank("na"), None, "a strict prefix does not rank");
            assert_eq!(table.rank("schemas"), None, "a superstring does not rank");
        }
        // An empty table of either kind ranks nothing, which is how "order alphabetically" is spelled.
        assert_eq!(Table::Builtin(&[]).rank("name"), None);
        assert_eq!(Table::User(&[]).rank("name"), None);
    }

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
