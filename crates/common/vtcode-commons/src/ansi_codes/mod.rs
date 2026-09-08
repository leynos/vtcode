//! Shared ANSI escape sequence constants and small builders for VT Code.
//!
//! See `docs/reference/ansi-in-vtcode.md` for the cross-crate integration map.

/// Escape character as a raw byte (ESC = 0x1B = 27)
pub const ESC_BYTE: u8 = 0x1b;

/// Escape character as a `char`
pub const ESC_CHAR: char = '\x1b';

/// Escape character as a string slice
pub const ESC: &str = "\x1b";

/// Control Sequence Introducer (CSI = ESC[)
pub const CSI: &str = "\x1b[";

/// Operating System Command (OSC = ESC])
pub const OSC: &str = "\x1b]";

/// Device Control String (DCS = ESC P)
pub const DCS: &str = "\x1bP";

/// String Terminator (ST = ESC \)
pub const ST: &str = "\x1b\\";

/// Bell character as a raw byte (BEL = 0x07)
pub(crate) const BEL_BYTE: u8 = 0x07;

/// Bell character as a `char`
pub const BEL_CHAR: char = '\x07';

/// Bell character as a string slice
const BEL: &str = "\x07";

/// Hyperlink — OSC 8
const OSC_HYPERLINK_PREFIX: &str = "\x1b]8;";

mod helpers;
mod notify;
mod styles;

pub use helpers::*;
pub use notify::*;
pub use styles::*;
