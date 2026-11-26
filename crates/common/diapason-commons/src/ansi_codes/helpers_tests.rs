//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn redraw_prefix_matches_cli_pattern() {
    assert_eq!(redraw_line_prefix(), "\r\x1b[2K");
}

#[test]
fn redraw_line_formats_expected_sequence() {
    assert_eq!(format_redraw_line("Done"), "\r\x1b[2KDone");
}
