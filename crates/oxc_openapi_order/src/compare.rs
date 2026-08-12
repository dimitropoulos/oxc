//! Key and segment comparison, matching JavaScript's string relational operators.
//!
//! Upstream compares keys with `a.toLowerCase() > b.toLowerCase()` and path segments with
//! bare `<` / `>`. JS string comparison orders by UTF-16 CODE UNIT, which is neither Rust's
//! `str: Ord` (UTF-8 byte order, i.e. code-point order) nor a `char`-wise comparison of the
//! lowercased text. The two disagree whenever a key mixes astral characters (U+10000 and
//! above, encoded as a surrogate pair whose lead unit is 0xD800..=0xDBFF) with characters in
//! U+E000..=U+FFFF:
//!
//! ```text
//! JS:   "\u{1F4A9}" < "\u{FFFD}"   because 0xD83D < 0xFFFD
//! Rust: "\u{1F4A9}" > "\u{FFFD}"   because U+1F4A9 > U+FFFD
//! ```
//!
//! Full case mapping matters for the same reason. `char::to_lowercase` is a full Unicode
//! mapping, so it can change a character's LENGTH, and that is observable in the ordering:
//!
//! - `Ä` lowercases to `ä` (U+00E4), which sorts AFTER `z` (U+007A).
//! - `İ` (U+0130) lowercases to `i` + U+0307, so it sorts BEFORE `z` on its first unit.
//! - `K` (U+212A, KELVIN SIGN) lowercases to `k`, TYING with a literal `k`.
//!
//! An ASCII-only comparison silently passes every ASCII test and gets all three wrong, so
//! the ASCII fast path here is taken only when BOTH sides are wholly ASCII. For pure ASCII,
//! byte order and UTF-16 code-unit order coincide, so the fast path is exact.
//!
//! One case needs more than per-character mapping: `String.prototype.toLowerCase` is
//! CONTEXT-SENSITIVE for U+03A3 (Greek capital sigma), which lowercases to `ς` at the end of a
//! word and `σ` elsewhere (Unicode's `Final_Sigma`). `char::to_lowercase` cannot see context and
//! always answers `σ`. U+03A3 is the only code point where this happens — verified exhaustively
//! over every code point in four contexts — so it gets a narrow, allocating fallback and
//! everything else stays allocation-free.

use std::cmp::Ordering;

/// Greek capital sigma, the one code point whose lowercase mapping depends on context.
const CAPITAL_SIGMA: char = '\u{3a3}';

/// The UTF-16 code units of `c`, without allocating.
fn utf16_units(c: char) -> impl Iterator<Item = u16> {
    let mut buf = [0u16; 2];
    let len = c.encode_utf16(&mut buf).len();
    buf.into_iter().take(len)
}

/// The UTF-16 code units of `s` after full Unicode lowercasing.
fn lowercase_utf16(s: &str) -> impl Iterator<Item = u16> + '_ {
    s.chars().flat_map(char::to_lowercase).flat_map(utf16_units)
}

/// JS `a.toLowerCase() < b.toLowerCase()` and friends: full case mapping, then UTF-16
/// code-unit order.
pub fn cmp_lowercase(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a
            .bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()));
    }
    if a.contains(CAPITAL_SIGMA) || b.contains(CAPITAL_SIGMA) {
        // `Final_Sigma` needs the whole string, so fall back to the standard-library mapping,
        // which implements it. `cow_utils` is not an option here: this crate is deliberately
        // dependency-free, and a `Cow` would not help anyway — the mapping always changes these
        // strings, so the allocation is unavoidable. It is gated behind a code point that a real
        // OpenAPI key essentially never contains.
        #[expect(clippy::disallowed_methods, reason = "needs context-sensitive Final_Sigma")]
        return a.to_lowercase().encode_utf16().cmp(b.to_lowercase().encode_utf16());
    }
    lowercase_utf16(a).cmp(lowercase_utf16(b))
}

/// JS `a < b` on strings: UTF-16 code-unit order, case-sensitive.
///
/// Used for path segments and tags, which upstream compares without lowercasing.
pub fn cmp_code_units(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.as_bytes().cmp(b.as_bytes());
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{cmp_code_units, cmp_lowercase};

    #[test]
    fn ascii_is_case_insensitive() {
        assert_eq!(cmp_lowercase("bar", "Foo"), Ordering::Less);
        assert_eq!(cmp_lowercase("Foo", "foo"), Ordering::Equal);
        assert_eq!(cmp_lowercase("application/json", "Application/XML"), Ordering::Less);
        assert_eq!(cmp_lowercase("", "a"), Ordering::Less);
        assert_eq!(cmp_lowercase("", ""), Ordering::Equal);
    }

    // The three cases a naive ASCII-only or `char`-wise comparison gets wrong.
    // Each is the measured behaviour of `Object.keys(..).sort(propComparator([]))`.

    #[test]
    fn a_umlaut_sorts_after_z() {
        // "Ä".toLowerCase() == "ä" == U+00E4 > "z" == U+007A
        assert_eq!(cmp_lowercase("\u{c4}", "z"), Ordering::Greater);
        assert_eq!(cmp_lowercase("\u{c4}", "a"), Ordering::Greater);
    }

    #[test]
    fn dotted_capital_i_sorts_before_z() {
        // U+0130 lowercases to "i" + U+0307, so the first unit decides: 0x69 < 0x7A.
        assert_eq!(cmp_lowercase("\u{130}", "z"), Ordering::Less);
        // ... and it is longer than a bare "i", so it sorts after it.
        assert_eq!(cmp_lowercase("\u{130}", "i"), Ordering::Greater);
    }

    #[test]
    fn kelvin_sign_ties_with_k() {
        // U+212A lowercases to "k": a tie that only full case mapping produces.
        assert_eq!(cmp_lowercase("\u{212a}", "k"), Ordering::Equal);
        assert_eq!(cmp_lowercase("k", "\u{212a}"), Ordering::Equal);
    }

    #[test]
    fn astral_sorts_by_utf16_code_unit_not_code_point() {
        // JS: "\u{1F4A9}" < "\u{FFFD}" (0xD83D < 0xFFFD).
        // Rust `str: Ord` would say Greater (U+1F4A9 > U+FFFD).
        assert_eq!(cmp_lowercase("\u{1f4a9}", "\u{fffd}"), Ordering::Less);
        assert_eq!(cmp_code_units("\u{1f4a9}", "\u{fffd}"), Ordering::Less);
        assert_eq!(
            "\u{1f4a9}".cmp("\u{fffd}"),
            Ordering::Greater,
            "the Rust default really does disagree, which is why this module exists"
        );
    }

    #[test]
    fn final_sigma_is_context_sensitive() {
        // JS: "ΑΣ".toLowerCase() == "ας" (U+03C2, final form), so it sorts BEFORE "ασ"
        // (U+03C3) -- 0x3C2 < 0x3C3. A per-character mapping answers U+03C3 for both and
        // wrongly calls them equal.
        assert_eq!(cmp_lowercase("\u{391}\u{3a3}", "\u{391}\u{3c3}"), Ordering::Less);
        assert_eq!(cmp_lowercase("\u{391}\u{3c3}", "\u{391}\u{3a3}"), Ordering::Greater);
        // Non-final sigma keeps the ordinary mapping, so these two ARE equal.
        assert_eq!(
            cmp_lowercase("\u{391}\u{3a3}\u{391}", "\u{391}\u{3c3}\u{391}"),
            Ordering::Equal
        );
        // A lone sigma is not word-final either (nothing cased precedes it).
        assert_eq!(cmp_lowercase("\u{3a3}", "\u{3c3}"), Ordering::Equal);
    }

    #[test]
    fn sharp_s_lowercases_to_itself() {
        // "ß".toLowerCase() == "ß" (only UPPERcasing expands it), so it sorts after "ss".
        assert_eq!(cmp_lowercase("\u{df}", "ss"), Ordering::Greater);
    }

    #[test]
    fn code_units_are_case_sensitive() {
        // Path segments are compared without lowercasing: 'B' (0x42) < 'a' (0x61).
        assert_eq!(cmp_code_units("Beta", "alpha"), Ordering::Less);
        assert_eq!(cmp_code_units("beta", "alpha"), Ordering::Greater);
    }
}
