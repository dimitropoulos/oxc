//! A minimal JSON-shaped document model, plus the JavaScript object semantics the reference
//! implementation inherits from its representation.
//!
//! Order-preserving by construction: a mapping is a `Vec` of pairs, never a hash map, because
//! the whole point of this harness is to compare orders.

use std::fmt::Write as _;

/// A JSON value.
///
/// Scalars carry their type, not just their text, because the reference's root gate is JavaScript
/// truthiness of the `openapi` member, and the string `"0"` is truthy while the number `0` is not.
/// A single stringly-typed scalar variant cannot tell those apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Kept as text so the JSON output round-trips exactly.
    Number(String),
    String(String),
    Seq(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Seq(_) => "sequence",
            Value::Map(_) => "mapping",
        }
    }

    /// JavaScript truthiness, which is what the reference's `if (jsonObj.openapi)` gate tests.
    ///
    /// `Value::String("0")` is true (only the empty string is falsy) while `Value::Number("0")` is
    /// false, and an empty object or array is true.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(value) => *value,
            // `Number("0e0")` and `Number("0.000")` are falsy too, so parse rather than whitelist.
            Value::Number(text) => text.parse::<f64>().is_ok_and(|value| value != 0.0),
            Value::String(text) => !text.is_empty(),
            Value::Seq(_) | Value::Map(_) => true,
        }
    }

    /// Serialise to JSON, preserving entry order, for handing a corpus to the real
    /// `openapi-format` (see `validate_reference.mjs`).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(text) => out.push_str(text),
            Value::String(text) => write_json_string(text, out),
            Value::Seq(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Map(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_json_string(key, out);
                    out.push(':');
                    value.write_json(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Escape everything outside printable ASCII so the file is pure ASCII and no
            // encoding question can confuse the comparison.
            ch if !(' '..='~').contains(&ch) => {
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// The key JavaScript treats as an array index: a canonical decimal in `0..=4294967294`.
///
/// Measured against the reference: `"0"`, `"10"` and `"4294967294"` bucket; `"01"`, `"-1"`,
/// `"1.5"`, `"4294967295"`, `" 1"` and `""` do not.
pub fn array_index_key(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

/// The indices of `keys` in the order `Object.keys` enumerates them: array-index keys first in
/// ascending numeric order, then every other key in insertion order.
///
/// This is ECMAScript's `OrdinaryOwnPropertyKeys`, a property of the object representation rather
/// than of any ordering rule. It applies to every mapping the reference builds, including the
/// one its `prioritySort` returns, so it overrides even the table ranking.
pub fn js_key_order(keys: &[&str]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    // Stable, and keyed so that non-index keys all compare equal and keep insertion order.
    order.sort_by_key(|&index| array_index_key(keys[index]).unwrap_or(u32::MAX));
    order
}

/// Reorder `entries` per [`js_key_order`].
pub fn js_object_order(entries: &mut Vec<(String, Value)>) {
    let keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
    let order = js_key_order(&keys);
    if order.iter().enumerate().all(|(position, &index)| position == index) {
        return;
    }
    let mut taken: Vec<Option<(String, Value)>> = entries.drain(..).map(Some).collect();
    *entries = order
        .into_iter()
        .map(|index| taken[index].take().expect("js_key_order repeats no index"))
        .collect();
}

/// Apply [`js_object_order`] to every mapping in the tree.
///
/// The reference's input arrives as JavaScript objects, so this ordering is already baked in
/// before any rule runs. Its explicit `JSON.parse(JSON.stringify(..))` deep copy is therefore a
/// no-op for ordering, not the cause of it.
pub fn js_object_order_deep(value: &mut Value) {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        Value::Seq(items) => {
            for item in items {
                js_object_order_deep(item);
            }
        }
        Value::Map(entries) => {
            js_object_order(entries);
            for (_, child) in entries.iter_mut() {
                js_object_order_deep(child);
            }
        }
    }
}
