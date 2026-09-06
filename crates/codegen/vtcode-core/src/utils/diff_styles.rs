// Diff colour palette for consistent git diff styling
// Centralizes RGB colour values for additions/deletions
//!
//! Re-exports from vtcode-commons for backward compatibility.

pub use vtcode_commons::styling::{
    DiffColourLevel, DiffColourPalette, DiffTheme, diff_add_bg, diff_del_bg, diff_gutter_bg_add_light,
    diff_gutter_bg_del_light, diff_gutter_fg_light,
};
