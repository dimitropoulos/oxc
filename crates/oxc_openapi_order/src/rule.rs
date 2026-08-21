//! Which table orders a mapping's own entries.
//!
//! One question, asked once per mapping. See the crate docs for why this is not a
//! transcription of upstream's traversal.

use crate::config::KeyOrderEntry;
use crate::tables::{self, ALPHABETICAL, CHILD_ROLE_KEYS, ROOT, Table};

/// One step of a mapping's ancestry, from the document root down to and including the
/// mapping's own step.
///
/// [`Step::Index`] steps are load-bearing. Upstream's guards index the ancestry absolutely
/// (`path[0]`, `path[1]`, `path[3]`), and its own path includes array indices as segments, so
/// dropping them would shift every later index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step<'a> {
    /// An entry of a mapping, identified by its key text.
    Key(&'a str),
    /// An element of a sequence, identified by its position.
    Index(u32),
}

impl Step<'_> {
    /// The step for the element at `index`.
    ///
    /// Saturates rather than wrapping. No table names a millionth element, so every index past
    /// `u32::MAX` is equally unnamed and collapsing them is safe; wrapping would alias one onto a
    /// small index that a guard does name.
    pub fn index(index: usize) -> Self {
        Self::Index(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

/// The key-order tables: the built-ins, with user overrides layered over them.
///
/// An override replaces one table by name and leaves every other table intact. Upstream's
/// `--sortFile` replaces the whole set instead, so a sort file naming only `get` silently
/// disables `requestBody`, `responses`, `properties` (which stops child role working at all) and
/// even `root`. Layering makes the option an escape hatch for one table rather than an
/// all-or-nothing switch.
///
/// One consequence of layering: a built-in table can never be removed, so [`KeyOrder::table`]
/// answers `Some` for all fifteen built-in names no matter what.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyOrder<'a> {
    overrides: &'a [KeyOrderEntry],
}

impl<'a> KeyOrder<'a> {
    /// Layer `overrides` over the built-in tables.
    pub const fn new(overrides: &'a [KeyOrderEntry]) -> Self {
        Self { overrides }
    }

    /// The effective table for `key`, if it has one.
    ///
    /// An empty table means "order alphabetically"; `None` means "keep source order". Those are
    /// different answers.
    ///
    /// A repeated override name resolves to the last entry, matching what JSON object parsing
    /// would have produced for the same source.
    pub fn table(&self, key: &str) -> Option<Table<'a>> {
        if let Some(entry) = self.overrides.iter().rev().find(|entry| entry.key == key) {
            return Some(Table::User(&entry.fields));
        }
        tables::builtin(key).map(Table::Builtin)
    }
}

/// The ordering policy for one format run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options<'a> {
    /// Field-order tables.
    pub key_order: KeyOrder<'a>,
    /// Order the members of every direct member of the root `components` mapping
    /// alphabetically.
    ///
    /// Upstream spells this as a list of component-type names (`sortComponentsSet`), whose
    /// default is empty. `true` here means every string-keyed member, which is wider than "every
    /// standard component type": `components.anything` qualifies, exactly as it would upstream if
    /// the name were listed.
    ///
    /// A sequence element is deliberately not a member, even though upstream's segment names are
    /// strings, so listing `"0"` there does make the pass fire on the first element of a
    /// `components` sequence. That shape is not expressible in OpenAPI, and rule 2 already gives
    /// such an element the `components` table; see
    /// `a_components_sequence_element_takes_the_table_not_alphabetical`.
    ///
    /// Upstream drives a second behaviour from the same option that is deliberately not ported
    /// here; see the crate docs.
    pub components: bool,
    /// Order `properties` mappings under `components.schemas` alphabetically
    /// (upstream's `sortComponentsProps`).
    pub properties: bool,
}

/// A key at absolute index `index` of `path`.
///
/// `None` when the index is out of range, or when the step is a sequence index, which can
/// never equal a table name, so upstream's `path[i] === 'components'` style comparisons are
/// false for it either way.
///
/// The same conflation applies to table lookup, where it narrows rather than matches: upstream's
/// path segments are strings even for sequence indices, so a table literally named `"0"` would
/// order every sequence's first element. No built-in table has a numeric name, so this only bites
/// a `keyOrder` override with one, which is meaningless for OpenAPI.
fn key_at<'a>(path: &[Step<'a>], index: usize) -> Option<&'a str> {
    match path.get(index)? {
        Step::Key(key) => Some(key),
        Step::Index(_) => None,
    }
}

/// [`key_at`] counted back from the end: `0` is the node's own step, `1` its parent's.
fn key_back<'a>(path: &[Step<'a>], back: usize) -> Option<&'a str> {
    key_at(path, path.len().checked_sub(back + 1)?)
}

/// Would the node at `path` have its children ordered, rather than its own entries?
///
/// The port of upstream's `responses`/`schemas`/`properties` arm and its two guards. The
/// guards stop a schema property literally named `properties`, and inline example payloads,
/// from being reordered as if they were schemas.
///
/// Every input is read from the node's own path, so this is called with the mapping's path
/// to answer "am I in child role?" (rule 1) and with the parent's path to answer "does my
/// parent order me?" (rule 4).
fn is_child_role(order: &KeyOrder<'_>, path: &[Step<'_>]) -> bool {
    let Some(key) = key_back(path, 0) else { return false };
    CHILD_ROLE_KEYS.contains(&key)
        // Upstream wraps all three arms in `sortSet.hasOwnProperty(this.key)`, so a replacement
        // sort file lacking `properties` really does switch child role off. Layered overrides
        // (see `KeyOrder`) can never remove a built-in, so this cannot currently be false. It
        // stays because the arm it ports has it, not because it fires.
        && order.table(key).is_some()
        // `this.parent.key !== 'properties' && this.parent.key !== 'value'`.
        // A parent that is a sequence element has no key, which passes.
        && !matches!(key_back(path, 1), Some("properties" | "value"))
        // `this.path[1] !== 'examples'`. Absolute index 1; out of range passes, which is
        // what a container at depth 1 gets.
        && key_at(path, 1) != Some("examples")
}

/// `components.examples.*.value` and everything below it is left alone.
///
/// Absolute indices, exactly as upstream: index 3 stays `value` however deep the node is.
fn is_under_components_examples_value(path: &[Step<'_>]) -> bool {
    key_at(path, 0) == Some("components")
        && key_at(path, 1) == Some("examples")
        && key_at(path, 3) == Some("value")
}

/// Is this sequence item inside an `example` payload whose elements upstream skips?
///
/// The guard is evaluated on the enclosing sequence, so drop the item's own index step
/// first. The asymmetry is real: an `example` array under a `requestBody` is skipped, the
/// same array under a `responses` is not.
fn is_skipped_example_element(path: &[Step<'_>]) -> bool {
    let Some(len) = path.len().checked_sub(1) else { return false };
    let sequence = &path[..len];
    key_back(sequence, 1) == Some("example")
        && (key_at(sequence, 0) == Some("components") || key_at(sequence, 3) == Some("requestBody"))
}

/// The table that orders the entries of the mapping at `path`, or `None` to keep source
/// order.
///
/// `path` ends with the mapping's own step. An empty `path` is the document root, which is
/// never ordered here: the root needs a content gate the caller owns, so it goes through
/// [`resolve_root`].
///
/// With `K` = this mapping's own key, `P` = the key of the mapping directly containing it,
/// and `S` = the key of the enclosing sequence when this mapping is a sequence item:
///
/// 1. `K` has a table, `K` is not in child role, and the mapping is not under
///    `components.examples.*.value` -> `K`'s table.
/// 2. Else the mapping is a sequence item and `S` has a table -> `S`'s table.
/// 3. Else an opt-in alphabetical pass applies -> alphabetical.
/// 4. Else `P` is in child role -> `P`'s table.
/// 5. Else source order.
///
/// Rule 1 beats rule 4 because upstream's traversal is pre-order: the parent writes its
/// child-arm order first, then the mapping's own visit overwrites it. Pinned by
/// `rule_1_beats_child_role_pre_order` below.
///
/// `P` is the directly containing mapping and is therefore `None` across a sequence
/// boundary. Upstream's child arm skips array-valued children, so a mapping inside a
/// sequence is reachable only by rule 2; reading `P` as "nearest enclosing mapping" instead
/// diverges on real documents.
///
/// NOTE: this answers the key-order question only. A mapping keyed `paths` is additionally
/// subject to [`PathsOrder`](crate::PathsOrder), which upstream applies in the mapping's own
/// visit and therefore after anything here; see [`is_paths_mapping`].
pub fn resolve<'a>(options: &Options<'a>, path: &[Step<'_>]) -> Option<Table<'a>> {
    let order = &options.key_order;

    // Rule 1: this mapping's own key orders it.
    if let Some(key) = key_back(path, 0)
        && let Some(table) = order.table(key)
        && !is_child_role(order, path)
        && !is_under_components_examples_value(path)
    {
        return Some(table);
    }

    // Rule 2: a sequence item is ordered by the sequence's key.
    if matches!(path.last(), Some(Step::Index(_))) {
        if let Some(key) = key_back(path, 1)
            && let Some(table) = order.table(key)
            && !is_skipped_example_element(path)
        {
            return Some(table);
        }
        // A sequence item's direct container is a sequence, not a mapping, so there is no `P`
        // and rule 4 cannot apply. Neither alphabetical pass can either: both name a mapping
        // key, and this step is an index.
        return None;
    }

    // Rule 3: the opt-in alphabetical passes. Upstream runs them in the mapping's own visit,
    // which happens after its parent's; and its child arm never writes its own node's key
    // order (it deep-copies the node, preserving the incoming order, and sorts only the node's
    // children). So an alphabetical write survives a rule-4 write.
    if let Some(table) = alphabetical(options, path) {
        return Some(table);
    }

    // Rule 4: the containing mapping is in child role and orders us.
    if let Some(parent_key) = key_back(path, 1) {
        let parent = &path[..path.len() - 1];
        if is_child_role(order, parent) {
            return order.table(parent_key);
        }
    }

    None
}

/// The two opt-in alphabetical passes (rule 3).
///
/// Both fire in the mapping's own visit upstream, and both lose to that same visit's later
/// generic block, but only where the generic block writes this node's own order, i.e. rule 1.
/// Verified against the reference:
///
/// - `components.parameters` with `components` on comes out in the `parameters` table order,
///   not alphabetically: rule 1 wins.
/// - `components.schemas.properties` with `properties` on comes out alphabetically, not in the
///   `schemas` table order: the parent's child-arm write (rule 4) loses, because that arm only
///   sorts a node's children and leaves the node's own order as it found it.
fn alphabetical<'a>(options: &Options<'a>, path: &[Step<'_>]) -> Option<Table<'a>> {
    // `sortComponentsSet`: a direct member of the root `components` mapping.
    //
    // `key_back(path, 0).is_some()` ports `sortComponentsSet.includes(this.key)`: a component
    // type is named by a string key, so a sequence element (whose step is an index) never
    // qualifies. Rule 2 already claims that shape, since `components` always has a table and
    // overrides can only replace one, never remove it, but the two functions should not have to
    // agree for this one to be right.
    if options.components
        && key_back(path, 0).is_some()
        && key_at(path, 0) == Some("components")
        && key_back(path, 1) == Some("components")
    {
        return Some(Table::Builtin(ALPHABETICAL));
    }

    // `sortComponentsProps`: any `properties` mapping under `components.schemas`, at any
    // depth (upstream checks only the key and absolute indices 0 and 1).
    if options.properties
        && key_back(path, 0) == Some("properties")
        && key_at(path, 0) == Some("components")
        && key_at(path, 1) == Some("schemas")
    {
        return Some(Table::Builtin(ALPHABETICAL));
    }

    None
}

/// Is this the `paths` mapping, whose members [`PathsOrder`](crate::PathsOrder) reorders?
///
/// Upstream gates its paths pass on the key alone, with no path constraint, so a mapping named
/// `paths` at any depth participates, including a schema property called `paths`. Callers get
/// the predicate from here so every backend agrees.
///
/// The paths pass takes precedence over [`resolve`]'s answer for the same mapping: upstream
/// runs it in the mapping's own visit, before the generic block, but the generic block's self
/// arm cannot fire for `paths` (it has no table), so nothing overwrites it.
pub fn is_paths_mapping(path: &[Step<'_>]) -> bool {
    key_back(path, 0) == Some("paths")
}

/// The table that orders the document root's entries.
///
/// `is_openapi_root` is the caller's content gate, and this crate does not pick its spelling.
/// Both formatter backends pass key presence, which is also what [`Session::ordering`] documents;
/// see [`crate::SortOpenapi`] and the crate docs for why presence rather than upstream's
/// truthiness. `swagger: "2.0"` matches under neither spelling, because the root table is
/// 3.x-shaped.
///
/// This is the only place upstream orders the root: the root node's own key is `undefined`, so
/// no traversal arm can fire for it.
///
/// [`Session::ordering`]: crate::Session::ordering
pub fn resolve_root<'a>(options: &Options<'a>, is_openapi_root: bool) -> Option<Table<'a>> {
    if !is_openapi_root {
        return None;
    }
    options.key_order.table(ROOT)
}

#[cfg(test)]
mod tests {
    use super::{KeyOrder, Options, Step, Table, is_paths_mapping, resolve, resolve_root};
    use crate::config::KeyOrderEntry;

    /// `keyOrder` overrides from a terser literal form.
    fn overrides(entries: &[(&str, &[&str])]) -> Vec<KeyOrderEntry> {
        entries
            .iter()
            .map(|(key, fields)| KeyOrderEntry {
                key: (*key).to_string(),
                fields: fields.iter().map(|field| (*field).to_string()).collect(),
            })
            .collect()
    }

    /// The table `resolve` picked, named by its first entry.
    fn described(table: Option<Table<'_>>) -> Option<&str> {
        table.map(Table::describe)
    }

    /// The built-in table `name`, named the same way [`described`] names one, so an assertion can say
    /// "this resolved to the `responses` table" without reaching into the table itself.
    fn builtin_table(name: &str) -> Option<&'static str> {
        crate::tables::builtin(name).map(|table| Table::Builtin(table).describe())
    }

    /// `a.b.c` -> `[Key("a"), Key("b"), Key("c")]`, with `#n` meaning `Index(n)`.
    fn path(spec: &str) -> Vec<Step<'_>> {
        if spec.is_empty() {
            return Vec::new();
        }
        spec.split('.')
            .map(|step| {
                step.strip_prefix('#')
                    .map_or(Step::Key(step), |index| Step::Index(index.parse().unwrap()))
            })
            .collect()
    }

    fn table_for(spec: &str) -> Option<Table<'static>> {
        resolve(&Options::default(), &path(spec))
    }

    fn first_of(spec: &str) -> Option<&'static str> {
        table_for(spec).map(Table::describe)
    }

    #[test]
    fn rule_1_self_table() {
        assert_eq!(first_of("paths./p.get"), Some("operationId"));
        assert_eq!(first_of("paths./p.post.requestBody"), Some("description"));
        assert_eq!(first_of("components"), Some("parameters"));
        // An empty table is still a table: `content` orders alphabetically.
        assert_eq!(
            described(table_for("paths./p.get.responses.200.content")),
            Some("<alphabetical>")
        );
    }

    #[test]
    fn rule_2_sequence_item_takes_the_sequence_key_table() {
        assert_eq!(first_of("paths./p.get.parameters.#0"), Some("name"));
        assert_eq!(first_of("paths./p.get.parameters.#7"), Some("name"));
    }

    #[test]
    fn rule_2_nested_sequence_has_no_key() {
        // The inner sequence's own step is an index, so it names no table.
        assert_eq!(table_for("paths./p.get.parameters.#0.#0"), None);
    }

    #[test]
    fn a_child_role_key_does_not_order_itself() {
        // `responses` orders its children, so its own entries (status codes) are untouched.
        assert_eq!(table_for("paths./p.get.responses"), None);
        assert_eq!(table_for("components.schemas"), None);
        assert_eq!(table_for("components.schemas.Pet.properties"), None);
    }

    #[test]
    fn rule_4_a_parent_in_child_role_orders_the_mapping() {
        // `responses > properties` means the `properties` mapping's own entries take the
        // `responses` table.
        assert_eq!(first_of("paths./p.get.responses.properties"), Some("description"));
        assert_eq!(
            described(table_for("paths./p.get.responses.properties")),
            builtin_table("responses")
        );
        // And a status-code mapping is ordered by the response table too.
        assert_eq!(first_of("paths./p.get.responses.200"), Some("description"));
        // A schema under `components.schemas` takes the schemas table.
        assert_eq!(first_of("components.schemas.Pet"), Some("description"));
    }

    #[test]
    fn rule_4_parent_must_be_the_direct_container() {
        // Across a sequence boundary there is no containing mapping, so rule 4 cannot reach
        // through one. A naive "nearest keyed ancestor mapping" reading would wrongly apply
        // the `schemas` table here; measured against the reference, `schemas[0].inner` comes
        // out in source order.
        assert_eq!(table_for("components.schemas.#0.inner"), None);
        // Same shape under a child-role key whose child is a sequence: the reference's child
        // arm skips array-valued children, so nothing orders the element's members.
        assert_eq!(table_for("x.responses.a.#0"), None);
        assert_eq!(table_for("x.responses.a.#0.inner"), None);
    }

    #[test]
    fn a_child_role_key_holding_a_sequence_orders_its_elements() {
        // The reference checks `Array.isArray(node)` before the child-role arm, so a child-role
        // key whose value is a sequence takes the array arm and its elements get the table.
        // Rule 1 cannot fire for an index step, so rule 2 reproduces this for free.
        // Measured: `components: {schemas: [{format, description, type, zz}]}` yields
        // [description, type, format, zz] for the element.
        assert_eq!(described(table_for("components.schemas.#0")), builtin_table("schemas"));
        assert_eq!(first_of("components.schemas.#0"), Some("description"));
    }

    #[test]
    fn rule_1_beats_child_role_pre_order() {
        // The discriminator. `content` under `components.schemas.S.properties` is reachable
        // from two visits: its own (rule 1, the empty `content` table -> alphabetical) and
        // its parent's (rule 4, the `properties` table). Upstream's traversal is pre-order,
        // so the mapping's own visit runs last and wins.
        //
        // Measured against openapi-format v1.33.6 with
        //   {openapi: "3.0.0", components: {schemas: {S: {properties:
        //     {content: {type: t, description: d, example: 1}}}}}}
        // which yields [description, example, type] -- alphabetical, i.e. rule 1.
        // The `properties` table would have given [description, type, example].
        assert_eq!(
            described(table_for("components.schemas.S.properties.content")),
            Some("<alphabetical>"),
            "rule 1 must win: the reference is pre-order"
        );
    }

    #[test]
    fn child_role_guard_parent_is_properties_or_value() {
        // `properties.schemas` -> the guard fails, so `schemas` is not in child role and
        // self-orders instead.
        assert_eq!(first_of("components.schemas.S.properties.schemas"), Some("description"));
        assert_eq!(first_of("x.value.properties"), Some("description"));
    }

    #[test]
    fn child_role_guard_second_segment_is_examples() {
        // `path[1] === 'examples'` suppresses child role, so the trio key self-orders.
        assert_eq!(first_of("x.examples.properties"), Some("description"));
        // ... and its children are therefore not ordered by it.
        assert_eq!(table_for("x.examples.properties.child"), None);
    }

    #[test]
    fn child_role_container_at_depth_one_has_no_path_index_1() {
        // A root-level `schemas` key: `this.path[1]` is undefined, so the guard passes and
        // child role applies.
        assert_eq!(first_of("schemas.A"), Some("description"));
        assert_eq!(table_for("schemas"), None);
    }

    #[test]
    fn components_examples_value_is_untouched() {
        assert_eq!(table_for("components.examples.Foo.value.schema"), None);
        // Any depth: absolute index 3 stays `value`.
        assert_eq!(table_for("components.examples.Foo.value.a.b.schema"), None);
        // A sibling that is not `value` is still ordered.
        assert_eq!(first_of("components.examples.Foo.notvalue.schema"), Some("description"));
        // Inline (non-`components`) examples are ordered.
        assert_eq!(
            first_of("paths./p.get.responses.200.content.app/json.examples.Foo.value.schema"),
            Some("description")
        );
    }

    #[test]
    fn example_array_elements_are_skipped_under_components_or_request_body() {
        // Under `components`: skipped.
        assert_eq!(
            table_for("components.requestBodies.RB.content.app/json.example.parameters.#0"),
            None
        );
        // `path[3] === 'requestBody'`: skipped.
        assert_eq!(
            table_for("paths./p.post.requestBody.content.app/json.example.parameters.#0"),
            None
        );
        // Under a response, the same shape is ordered.
        assert_eq!(
            first_of("paths./p.post.responses.200.content.app/json.example.parameters.#0"),
            Some("name")
        );
    }

    #[test]
    fn root_is_never_resolved_by_the_general_rule() {
        assert_eq!(table_for(""), None);
        assert_eq!(resolve_root(&Options::default(), false), None);
        assert_eq!(resolve_root(&Options::default(), true).map(Table::describe), Some("openapi"));
    }

    #[test]
    fn a_mapping_literally_keyed_root_gets_the_root_table() {
        // Faithful to upstream, which keys tables by name with no path constraint.
        assert_eq!(first_of("components.schemas.S.properties.root"), Some("openapi"));
    }

    #[test]
    fn key_order_overrides_replace_one_table_and_leave_the_rest() {
        let over = overrides(&[("get", &["summary", "operationId"])]);
        let options = Options { key_order: KeyOrder::new(&over), ..Options::default() };
        assert_eq!(resolve(&options, &path("paths./p.get")).map(Table::describe), Some("summary"));
        // Untouched tables still resolve.
        assert_eq!(
            resolve(&options, &path("paths./p.post")).map(Table::describe),
            Some("operationId")
        );
        assert_eq!(
            resolve(&options, &path("x.requestBody")).map(Table::describe),
            Some("description")
        );
    }

    #[test]
    fn components_option_orders_component_types_alphabetically() {
        let options = Options { components: true, ..Options::default() };
        // `components.schemas` is in child role, so no table applies -> alphabetical.
        assert_eq!(
            described(resolve(&options, &path("components.schemas"))),
            Some("<alphabetical>")
        );
        assert_eq!(
            described(resolve(&options, &path("components.responses"))),
            Some("<alphabetical>")
        );
        // But a component type whose key has a table keeps the table (rule 1 wins).
        // Verified against the reference, which yields [name, in, schema, aaa, zzz].
        assert_eq!(
            resolve(&options, &path("components.parameters")).map(Table::describe),
            Some("name")
        );
        // Off by default.
        assert_eq!(table_for("components.schemas"), None);
        // Only a direct member of the root `components` mapping.
        assert_eq!(resolve(&options, &path("x.components.schemas")), None);
    }

    #[test]
    fn a_components_sequence_element_takes_the_table_not_alphabetical() {
        // A deliberate narrowing, pinned with evidence that tells the two readings apart.
        //
        // Upstream's path segments are strings even for sequence indices, so listing `"0"` in
        // `sortComponentsSet` makes its components pass fire on the first element of a `components`
        // sequence. Measured on openapi-format v1.33.6 with
        //   {openapi: "3.0.0", components: [{mediaTypes: 1, schemas: 2}]}
        //   sortComponentsSet: ["0"]       -> [mediaTypes, schemas]  (alphabetical)
        //   sortComponentsSet: ["schemas"] -> [schemas, mediaTypes]  (the components table)
        // The keys were chosen so the two readings disagree; a pair of unranked keys could not tell
        // them apart.
        //
        // We take the table reading: an index is not a member name, and a `components` sequence is
        // not expressible in OpenAPI anyway.
        let options = Options { components: true, ..Options::default() };
        assert_eq!(
            described(resolve(&options, &path("components.#0"))),
            builtin_table("components")
        );
        assert_ne!(described(resolve(&options, &path("components.#0"))), Some("<alphabetical>"));
    }

    #[test]
    fn properties_option_orders_component_schema_properties() {
        let options = Options { properties: true, ..Options::default() };
        assert_eq!(
            described(resolve(&options, &path("components.schemas.Pet.properties"))),
            Some("<alphabetical>")
        );
        // Any depth, per upstream's key + absolute-index test.
        assert_eq!(
            described(resolve(&options, &path("components.schemas.Pet.properties.a.properties"))),
            Some("<alphabetical>")
        );
        // Outside `components.schemas`, untouched.
        assert_eq!(resolve(&options, &path("paths./p.get.properties")), None);
        // Off by default.
        assert_eq!(table_for("components.schemas.Pet.properties"), None);
    }

    #[test]
    fn an_alphabetical_pass_beats_child_role() {
        // A schema literally named `properties`: rule 4 (its parent `schemas` is in child role)
        // would give the schemas table, but the alphabetical pass fires in the mapping's own
        // visit and the child arm never writes its own node's order, so alphabetical wins.
        //
        // Measured against openapi-format v1.33.6 with `sortComponentsProps: true` on
        //   {openapi: "3.0.0", components: {schemas: {properties:
        //     {zz: 1, description: 2, type: 3, aa: 4}}}}
        // which yields [aa, description, type, zz] -- alphabetical. With the option off the
        // same document yields [description, type, aa, zz] -- the schemas table.
        let options = Options { properties: true, ..Options::default() };
        assert_eq!(
            described(resolve(&options, &path("components.schemas.properties"))),
            Some("<alphabetical>")
        );
        assert_eq!(
            described(resolve(&options, &path("components.schemas.S.responses.properties"))),
            Some("<alphabetical>")
        );
        // With the option off, rule 4 applies again.
        assert_eq!(described(table_for("components.schemas.properties")), builtin_table("schemas"));
        // Rule 1 still beats the alphabetical pass: `components.parameters` keeps its table.
        let options = Options { components: true, ..Options::default() };
        assert_eq!(
            resolve(&options, &path("components.parameters")).map(Table::describe),
            Some("name")
        );
    }

    #[test]
    fn paths_mapping_is_recognised_at_any_depth() {
        // Upstream gates its paths pass on the key alone, with no path constraint.
        assert!(is_paths_mapping(&path("paths")));
        assert!(is_paths_mapping(&path("components.schemas.S.properties.paths")));
        assert!(!is_paths_mapping(&path("paths./p")));
        assert!(!is_paths_mapping(&path("")));
        assert!(!is_paths_mapping(&path("paths.#0")));
    }

    #[test]
    fn a_repeated_override_name_resolves_to_the_last() {
        let over = overrides(&[("get", &["first"]), ("get", &["last"])]);
        let options = Options { key_order: KeyOrder::new(&over), ..Options::default() };
        assert_eq!(described(resolve(&options, &path("x.get"))), Some("last"));
    }
}
