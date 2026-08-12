//! What `sortOpenapi` accepts, owned rather than borrowed.
//!
//! This is the user's side of the crate; [`Options`] is the rule's side. They are separate types
//! because they have different owners and different lifetimes: a formatter holds this for the whole
//! run, while [`Options`] is a borrowed view built for the length of one question.
//!
//! Both formatter backends embed [`SortOpenapi`] directly, so the option means exactly one thing in
//! both and the defaults cannot drift apart.

use crate::{KeyOrder, Options, PathsOrder};

/// One `keyOrder` override: a parent key, and the field order to use for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOrderEntry {
    /// The parent key whose table this replaces: `"get"`, `"responses"`, `"root"`, and so on.
    pub key: String,
    /// The field order. Keys not listed follow the listed ones, compared case-insensitively.
    ///
    /// An empty list is a real answer rather than an omission: it ranks nothing, so the mapping is
    /// ordered alphabetically. Leaving the entry out instead keeps the built-in table, or source
    /// order.
    pub fields: Vec<String>,
}

/// `sortOpenapi`: whether to order OpenAPI documents, and how.
///
/// [`Default`] is enabled with every sub-option at its own default, which is the default pass and
/// exactly what upstream does with no flags. The option is on by default, so the type's default has
/// to be the on state or the two would disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortOpenapi {
    /// Whether to order at all.
    pub enabled: bool,
    /// How the members of the root `paths` mapping are ordered.
    pub paths: PathsOrder,
    /// Order the members of every direct member of the root `components` mapping alphabetically.
    pub components: bool,
    /// Order `properties` mappings under `components.schemas` alphabetically.
    pub properties: bool,
    /// Field-order overrides, layered over the built-in tables.
    ///
    /// Naming one table leaves every other one intact. This is a deliberate difference from
    /// upstream's `--sortFile`, which replaces the whole set, so that naming only `get` there
    /// silently disables `requestBody`, `responses` and `properties` as well.
    pub key_order: Vec<KeyOrderEntry>,
}

impl Default for SortOpenapi {
    fn default() -> Self {
        Self {
            enabled: true,
            paths: PathsOrder::default(),
            components: false,
            properties: false,
            key_order: Vec::new(),
        }
    }
}

impl From<bool> for SortOpenapi {
    /// `true` is the default pass; `false` disables the feature entirely.
    fn from(enabled: bool) -> Self {
        Self { enabled, ..Self::default() }
    }
}

impl SortOpenapi {
    /// The borrowed view the rule resolves against.
    ///
    /// Cheap enough to build per mapping: it borrows the overrides rather than copying them, which
    /// is the reason [`crate::Table`] has an owned representation at all.
    pub fn options(&self) -> Options<'_> {
        Options {
            key_order: KeyOrder::new(&self.key_order),
            components: self.components,
            properties: self.properties,
        }
    }
}
