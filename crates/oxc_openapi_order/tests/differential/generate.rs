//! A seeded generator of OpenAPI-shaped documents, biased towards the shapes the ordering rule
//! actually branches on.
//!
//! Random documents are nearly useless here: the interesting behaviour lives in a handful of key
//! names and their positions. So the vocabulary is exactly the guard vocabulary, and the shapes are
//! built to plant those names at the depths where the absolute path indices matter.

use crate::differential::value::Value;

/// A small deterministic PRNG (xorshift64*), so the corpus is reproducible without a dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Any non-zero state will do; mix the seed so adjacent seeds diverge immediately.
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0x2545_f491_4f6c_dd1d) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        assert!(bound > 0);
        usize::try_from(self.next() % bound as u64).unwrap_or(0)
    }

    fn pick<'v, T>(&mut self, values: &'v [T]) -> &'v T {
        &values[self.below(values.len())]
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

/// Key names that steer the rule: table names, guard names, and the trio.
const STRUCTURAL: [&str; 19] = [
    "components",
    "examples",
    "example",
    "value",
    "schemas",
    "schema",
    "properties",
    "responses",
    "requestBody",
    "parameters",
    "content",
    "paths",
    "get",
    "query",
    "post",
    "put",
    "patch",
    "delete",
    "root",
];

/// Key names that are only ever ranked or unranked, never structural.
const FIELDS: [&str; 20] = [
    "operationId",
    "summary",
    "description",
    "type",
    "items",
    "format",
    "default",
    "enum",
    "name",
    "in",
    "required",
    "headers",
    "links",
    "tags",
    "info",
    "servers",
    "openapi",
    "mediaTypes",
    "x-tagGroups",
    "externalDocs",
];

/// Keys chosen to stress the comparator and the JavaScript object semantics.
const AWKWARD: [&str; 18] = [
    "zzz",
    "aaa",
    "Type",
    "type",
    "TYPE",
    "\u{c4}",   // Ä  -> lowercases after z
    "\u{130}",  // İ  -> lowercases to i + U+0307
    "\u{212a}", // K  -> lowercases to k, tying
    "\u{3a3}",  // Σ  -> context-sensitive lowercase
    "\u{391}\u{3a3}",
    "\u{1f4a9}", // astral: UTF-16 order differs from code-point order
    "\u{fffd}",
    "__proto__", // dropped by the reference
    "0",
    "2",
    "10",
    "01",
    "",
];

/// Path-template-ish keys, for `paths` mappings.
const PATHS: [&str; 6] = ["/pets", "/pets/", "/pets/{petId}", "/a/b", "/a", "/Beta"];

/// A scalar of a randomly chosen JSON type.
///
/// The type matters for the root gate: `Value::String("0")` is truthy, `Value::Number("0")` is not.
fn scalar(rng: &mut Rng) -> Value {
    match rng.below(8) {
        0 => Value::Number("1".to_string()),
        1 => Value::Number("0".to_string()),
        2 => Value::String("text".to_string()),
        3 => Value::String(String::new()),
        4 => Value::Bool(true),
        5 => Value::Bool(false),
        6 => Value::Null,
        _ => Value::String("3.0.0".to_string()),
    }
}

/// A key for a mapping at the given depth.
fn key(rng: &mut Rng, depth: usize) -> String {
    // Structural names matter most near the top, where the absolute indices land.
    let structural_odds = if depth < 4 { 2 } else { 4 };
    if rng.chance(structural_odds) {
        return (*rng.pick(&STRUCTURAL)).to_string();
    }
    if rng.chance(2) {
        return (*rng.pick(&FIELDS)).to_string();
    }
    if rng.chance(4) {
        return (*rng.pick(&PATHS)).to_string();
    }
    (*rng.pick(&AWKWARD)).to_string()
}

fn value(rng: &mut Rng, depth: usize, budget: &mut usize) -> Value {
    if depth >= 7 || *budget == 0 {
        return scalar(rng);
    }
    match rng.below(10) {
        0..=5 => map(rng, depth, budget),
        6..=7 => seq(rng, depth, budget),
        _ => scalar(rng),
    }
}

fn seq(rng: &mut Rng, depth: usize, budget: &mut usize) -> Value {
    let len = 1 + rng.below(3);
    let mut items = Vec::with_capacity(len);
    for _ in 0..len {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        // A sequence directly inside a sequence is the shape that makes the reference rewrite a
        // node's kind, so make it reachable rather than rare.
        if rng.chance(4) {
            items.push(seq(rng, depth + 1, budget));
        } else {
            items.push(value(rng, depth + 1, budget));
        }
    }
    Value::Seq(items)
}

fn map(rng: &mut Rng, depth: usize, budget: &mut usize) -> Value {
    let len = 1 + rng.below(6);
    let mut entries: Vec<(String, Value)> = Vec::with_capacity(len);
    for _ in 0..len {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let name = key(rng, depth);
        // A mapping cannot hold the same key twice, and the reference's object model could not
        // represent it anyway.
        if entries.iter().any(|(existing, _)| *existing == name) {
            continue;
        }
        let child = value(rng, depth + 1, budget);
        entries.push((name, child));
    }
    Value::Map(entries)
}

/// A mapping whose key order distinguishes every table the rule can resolve.
///
/// `description`/`type` separate the schema tables, `operationId` the operation tables, `name` the
/// parameter table, `enum` the properties table, `content` the response table, and a mapping with
/// none of them ranked comes out alphabetically. Without a probe like this, a mutation that swaps one
/// table for another produces the same bytes and nothing notices.
fn probe() -> Value {
    Value::Map(
        ["zzz", "enum", "type", "name", "operationId", "content", "description", "aaa"]
            .into_iter()
            .map(|key| (key.to_string(), Value::Number("1".to_string())))
            .collect(),
    )
}

/// A probe for tie order and for context-sensitive lowercasing.
///
/// `ΑΣ` and `Ασ` lowercase to `ας` and `ασ`, which differ only if `Final_Sigma` is applied; without
/// it they tie and keep source order. `TYPE` and `type` tie under any correct comparator, so their order
/// pins the tiebreak.
fn tie_probe() -> Value {
    Value::Map(
        ["\u{391}\u{3c3}", "TYPE", "\u{391}\u{3a3}", "z", "type"]
            .into_iter()
            .map(|key| (key.to_string(), Value::Number("1".to_string())))
            .collect(),
    )
}

/// Nest `leaf` under `path`, innermost last.
fn chain(path: &[&str], leaf: Value) -> (String, Value) {
    let (first, rest) = path.split_first().expect("a chain needs at least one step");
    let inner = if rest.is_empty() { leaf } else { Value::Map(vec![chain(rest, leaf)]) };
    ((*first).to_string(), inner)
}

/// Exact chains that random generation cannot reach.
///
/// Every guard in the rule keys on an absolute ancestry index, so reaching one means hitting a
/// specific 4-or-5 step chain, around one in fifty thousand per position triple with this
/// vocabulary. Planting them is the difference between a corpus that exercises the guards and one
/// that merely contains their names.
fn planted(index: usize) -> (String, Value) {
    let seq = |leaf: Value| Value::Seq(vec![leaf]);
    match index % 16 {
        // The `components.examples.*.value` exclusion, and two shapes where it must not fire.
        0 => chain(&["components", "examples", "E", "value", "schema"], probe()),
        1 => chain(&["components", "examples", "E", "value", "a", "b", "schema"], probe()),
        2 => chain(&["components", "examples", "E", "notvalue", "schema"], probe()),
        // The skipped `example` sequence, under `components` and under `path[3] == requestBody` ...
        3 => chain(
            &["components", "requestBodies", "RB", "content", "app", "example", "parameters"],
            seq(probe()),
        ),
        4 => chain(
            &["paths", "/p", "post", "requestBody", "content", "app", "example", "parameters"],
            seq(probe()),
        ),
        // ... and the same shape under a response, where it must not be skipped.
        5 => chain(
            &["paths", "/p", "post", "responses", "200", "content", "app", "example", "parameters"],
            seq(probe()),
        ),
        // An alphabetical pass against child role.
        6 => chain(&["components", "schemas", "properties"], probe()),
        7 => chain(&["components", "schemas", "S", "responses", "properties"], probe()),
        8 => chain(&["components", "schemas", "S", "properties"], probe()),
        // Rule 1 against child role, and rule 1 against the components pass.
        9 => chain(&["components", "schemas", "S", "properties", "content"], probe()),
        10 => chain(&["components", "parameters"], probe()),
        // The child-role guard on absolute index 1.
        11 => chain(&["x", "examples", "properties", "child"], probe()),
        // Tables that the random vocabulary rarely lands on a mapping.
        12 => chain(&["paths", "/p", "query"], probe()),
        13 => chain(&["paths", "/p", "get", "parameters"], seq(probe())),
        // Tie order and Final_Sigma, in a mapping whose parent is in child role.
        14 => chain(&["x", "schemas", "content"], tie_probe()),
        _ => chain(&["components", "schemas", "S", "properties", "Type"], tie_probe()),
    }
}

/// One document for `seed`.
///
/// Most documents carry a root `openapi` member so the root pass is exercised; the rest omit it, or
/// give it a falsy value, so the truthiness gate is exercised too. Half carry a planted chain.
pub fn document(seed: u64) -> Value {
    let rng = &mut Rng::new(seed);
    let mut budget = 60;
    let Value::Map(mut entries) = map(rng, 0, &mut budget) else { unreachable!() };
    entries.retain(|(key, _)| key != "openapi");

    if seed.is_multiple_of(2) {
        let (key, value) = planted(usize::try_from(seed / 2).unwrap_or(0));
        entries.retain(|(existing, _)| *existing != key);
        let at = rng.below(entries.len() + 1);
        entries.insert(at, (key, value));
    }

    // Every falsy JSON scalar gets a turn, so the truthiness gate is exercised in both directions.
    match rng.below(10) {
        0 => {}
        1 => entries.insert(rng.below(entries.len() + 1), ("openapi".into(), scalar(rng))),
        2 => entries.push(("openapi".into(), Value::String(String::new()))),
        3 => entries.push(("openapi".into(), Value::Number("0".into()))),
        4 => entries.push(("openapi".into(), Value::String("0".into()))),
        5 => entries.push(("openapi".into(), Value::Bool(false))),
        6 => entries.push(("openapi".into(), Value::Null)),
        7 => entries.push(("swagger".into(), Value::String("2.0".into()))),
        _ => entries.insert(0, ("openapi".into(), Value::String("3.0.0".into()))),
    }
    Value::Map(entries)
}
