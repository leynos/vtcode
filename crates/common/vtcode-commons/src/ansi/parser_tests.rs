//! Extracted regression tests; production source remains byte-identical.

use super::{CAN, SUB, strip_ansi, strip_ansi_ascii_only};

#[test]
fn strips_esc_csi_sequences() {
    let input = "a\x1b[31mred\x1b[0mz";
    assert_eq!(strip_ansi(input), "aredz");
    assert_eq!(strip_ansi_ascii_only(input), "aredz");
}

#[test]
fn utf8_encoded_c1_is_not_reprocessed_as_control() {
    // XTerm/ECMA-48: controls are processed once; decoded UTF-8 text is not reprocessed as C1.
    let input = "a\u{009b}31mred";
    assert_eq!(strip_ansi(input), input);
}

#[test]
fn strip_removes_ascii_del_control() {
    let input = format!("a{}b", char::from(0x7f));
    assert_eq!(strip_ansi(&input), "ab");
}

#[test]
fn csi_aborts_on_esc_then_new_sequence_parses() {
    let input = "a\x1b[31\x1b[32mgreen\x1b[0mz";
    assert_eq!(strip_ansi(input), "agreenz");
}

#[test]
fn csi_aborts_on_can_and_sub() {
    let can = format!("a\x1b[31{}b", char::from(CAN));
    let sub = format!("a\x1b[31{}b", char::from(SUB));
    assert_eq!(strip_ansi(&can), "ab");
    assert_eq!(strip_ansi(&sub), "ab");
}

#[test]
fn osc_aborts_on_esc_non_st() {
    let input = "a\x1b]title\x1b[31mred\x1b[0mz";
    assert_eq!(strip_ansi(input), "aredz");
}

#[test]
fn incomplete_sequence_drops_tail() {
    let input = "text\x1b[31";
    assert_eq!(strip_ansi(input), "text");
}

#[test]
fn ascii_only_incomplete_sequence_keeps_tail() {
    let input = "text\x1b[31";
    assert_eq!(strip_ansi_ascii_only(input), input);
}

#[test]
fn strips_common_progress_redraw_sequences() {
    // Common pattern for dynamic CLI updates:
    // carriage return + erase line + redraw text.
    let input = "\r\x1b[2KProgress 10%\r\x1b[2KDone\n";
    assert_eq!(strip_ansi(input), "\rProgress 10%\rDone\n");
}

#[test]
fn strips_cursor_navigation_sequences() {
    let input = "left\x1b[1D!\nup\x1b[1Arow";
    assert_eq!(strip_ansi(input), "left!\nuprow");
}

#[test]
fn strip_ansi_bytes_supports_raw_c1_csi() {
    let input = [b'a', 0x9b, b'3', b'1', b'm', b'r', b'e', b'd', 0x9b, b'0', b'm', b'z'];
    let out = super::strip_ansi_bytes(&input);
    assert_eq!(out, b"aredz");
}

#[test]
fn strip_ansi_bytes_supports_raw_c1_osc_and_st() {
    let mut input = b"pre".to_vec();
    input.extend_from_slice(&[0x9d]);
    input.extend_from_slice(b"8;;https://example.com");
    input.extend_from_slice(&[0x9c]);
    input.extend_from_slice(b"link");
    input.extend_from_slice(&[0x9d]);
    input.extend_from_slice(b"8;;");
    input.extend_from_slice(&[0x9c]);
    input.extend_from_slice(b"post");
    let out = super::strip_ansi_bytes(&input);
    assert_eq!(out, b"prelinkpost");
}

#[test]
fn csi_respects_parameter_intermediate_final_grammar() {
    // Parameter bytes ("1;2"), intermediate bytes (" "), then final ("m")
    let input = "a\x1b[1;2 mred\x1b[0mz";
    assert_eq!(strip_ansi(input), "aredz");
}

#[test]
fn malformed_csi_does_not_consume_following_text() {
    // 0x10 is not valid CSI parameter/intermediate/final.
    let malformed = format!("a\x1b[12{}visible", char::from(0x10));
    assert_eq!(strip_ansi(&malformed), "avisible");
}

#[test]
fn strips_wikipedia_sgr_8bit_colour_pattern() {
    let input = "x\x1b[38;5;196mred\x1b[0my";
    assert_eq!(strip_ansi(input), "xredy");
}

#[test]
fn strips_wikipedia_sgr_truecolour_pattern() {
    let input = "x\x1b[48;2;12;34;56mblock\x1b[0my";
    assert_eq!(strip_ansi(input), "xblocky");
}

#[test]
fn strips_wikipedia_osc8_hyperlink_pattern() {
    let input = "go \x1b]8;;https://example.com\x1b\\here\x1b]8;;\x1b\\ now";
    assert_eq!(strip_ansi(input), "go here now");
}

#[test]
fn strips_dec_private_mode_csi() {
    let input = "a\x1b[?25lb\x1b[?25hc";
    assert_eq!(strip_ansi(input), "abc");
}

#[test]
fn strips_three_byte_esc_sequences() {
    // ESC # 8 = DEC screen alignment test
    let input = "a\x1b#8b";
    assert_eq!(strip_ansi(input), "ab");

    // ESC ( B = designate US ASCII as G0
    let input2 = "a\x1b(Bb";
    assert_eq!(strip_ansi(input2), "ab");

    // ESC SP F = 7-bit controls
    let input3 = "a\x1b Fb";
    assert_eq!(strip_ansi(input3), "ab");

    // ESC % G = select UTF-8
    let input4 = "a\x1b%Gb";
    assert_eq!(strip_ansi(input4), "ab");
}

#[test]
fn incomplete_three_byte_esc_sequence_drops_tail() {
    // ESC # at end — incomplete, should not consume past end
    let input = "text\x1b#";
    assert_eq!(strip_ansi(input), "text");
}
