//! The stable id of a mutant.
//!
//! An id is the XXH3 hash of the file key, the items around the site, the text of
//! the site, the occurrence of that text in those items, the operator and the
//! replacement. The input has no line number, so an edit in another function does
//! not change the id. An edit that adds the same text earlier in the same function
//! does change it. The value 0 means "no active mutant", so the function never
//! returns it.

use xxhash_rust::xxh3::xxh3_64;

/// The inputs of one id.
#[derive(Debug, Clone, Copy)]
pub struct IdInput<'a> {
    /// The file path, relative to the workspace root, with `/` separators.
    pub file: &'a str,
    /// The items around the site.
    pub scope: &'a str,
    /// The text of the site.
    pub site_text: &'a str,
    /// The number of earlier sites with the same text in the same items.
    pub occurrence: u32,
    /// The operator name.
    pub operator: &'a str,
    /// The replacement text.
    pub replacement: &'a str,
}

/// The version of the input layout. A change of the layout changes this value.
const LAYOUT: u32 = 2;

/// Computes the id.
pub fn mutant_id(input: IdInput<'_>) -> u64 {
    let mut bytes = Vec::with_capacity(input.site_text.len() + 64);
    bytes.extend_from_slice(&LAYOUT.to_le_bytes());
    for part in [
        input.file,
        input.scope,
        input.site_text,
        input.operator,
        input.replacement,
    ] {
        // The length prefix keeps `ab` + `c` different from `a` + `bc`.
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    bytes.extend_from_slice(&input.occurrence.to_le_bytes());
    xxh3_64(&bytes).max(1)
}

/// The text form of an id: 16 lowercase hexadecimal digits.
pub fn id_text(id: u64) -> String {
    format!("{id:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn input(site_text: &str, occurrence: u32) -> IdInput<'_> {
        IdInput {
            scope: "fn f",
            file: "src/lib.rs",
            site_text,
            occurrence,
            operator: "relational-operator",
            replacement: "!=",
        }
    }

    /// A fixed value. A change here changes each saved id, so it must be a
    /// deliberate change of `LAYOUT`.
    #[test]
    fn the_value_is_stable() {
        assert_eq!(id_text(mutant_id(input("a == b", 0))), "2b667b0a24330022");
    }

    #[test]
    fn each_input_changes_the_id() {
        let base = mutant_id(input("a == b", 0));
        assert_ne!(base, mutant_id(input("a == c", 0)));
        assert_ne!(base, mutant_id(input("a == b", 1)));
        let other_file = IdInput {
            file: "src/a.rs",
            ..input("a == b", 0)
        };
        assert_ne!(base, mutant_id(other_file));
        let other_operator = IdInput {
            operator: "x",
            ..input("a == b", 0)
        };
        assert_ne!(base, mutant_id(other_operator));
        let other_replacement = IdInput {
            replacement: "<",
            ..input("a == b", 0)
        };
        assert_ne!(base, mutant_id(other_replacement));
    }

    #[test]
    fn a_part_boundary_changes_the_id() {
        let first = IdInput {
            operator: "ab",
            replacement: "c",
            ..input("x", 0)
        };
        let second = IdInput {
            operator: "a",
            replacement: "bc",
            ..input("x", 0)
        };
        assert_ne!(mutant_id(first), mutant_id(second));
    }

    #[test]
    fn id_text_has_a_fixed_width() {
        assert_eq!(id_text(0xab), "00000000000000ab");
    }
}
