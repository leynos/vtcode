//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn non_ascii_hex_is_rejected_without_panicking() {
    assert!(colour_from_hex("红色").is_none());
}
