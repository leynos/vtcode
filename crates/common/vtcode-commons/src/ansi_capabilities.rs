//! ANSI terminal capabilities detection and feature support

use crate::colour_policy::no_colour_env_active;
use once_cell::sync::Lazy;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU8, Ordering};

/// Check if `CLICOLOR` environment variable is set to a non-zero value.
fn clicolour() -> Option<bool> {
    match std::env::var("CLICOLOR") {
        Ok(val) => Some(!val.is_empty() && val != "0"),
        Err(_) => None,
    }
}

/// Check if `CLICOLOR_FORCE` environment variable is set to a non-zero value.
fn clicolour_force() -> bool {
    std::env::var("CLICOLOR_FORCE").is_ok_and(|val| !val.is_empty() && val != "0")
}

/// Check if the terminal supports ANSI color output.
fn term_supports_colour() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if let Ok(term) = std::env::var("TERM") {
        if term == "dumb" || term == "emacs" {
            return false;
        }
    }
    true
}

/// Color depth support level detected for the terminal
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColourDepth {
    /// No color support
    None = 0,
    /// 16 colours (basic ANSI)
    Basic16 = 1,
    /// 256 colours
    Colour256 = 2,
    /// True color (24-bit RGB)
    TrueColour = 3,
}

impl ColourDepth {
    /// Get a human-readable name for this color depth
    pub fn name(self) -> &'static str {
        match self {
            ColourDepth::None => "none",
            ColourDepth::Basic16 => "16-colour",
            ColourDepth::Colour256 => "256-colour",
            ColourDepth::TrueColour => "true-colour",
        }
    }

    /// Check if this depth supports color
    fn supports_colour(self) -> bool {
        self != ColourDepth::None
    }

    /// Check if this depth is at least 256 colours
    pub fn supports_256_colours(self) -> bool {
        self >= ColourDepth::Colour256
    }

    /// Check if this depth supports true color
    pub fn supports_true_colour(self) -> bool {
        self == ColourDepth::TrueColour
    }
}

/// ANSI terminal feature capabilities
#[derive(Clone, Copy, Debug)]
pub struct AnsiCapabilities {
    /// Detected color depth
    colour_depth: ColourDepth,
    /// Whether unicode is supported
    pub unicode_support: bool,
    /// Whether to force color output
    pub force_colour: bool,
    /// Whether color is explicitly disabled
    pub no_colour: bool,
}

impl AnsiCapabilities {
    /// Detect terminal capabilities
    pub fn detect() -> Self {
        Self {
            colour_depth: detect_colour_depth(),
            unicode_support: detect_unicode_support(),
            force_colour: clicolour_force(),
            no_colour: no_colour_env_active(),
        }
    }

    /// Check if color output is supported
    pub fn supports_colour(&self) -> bool {
        !self.no_colour && (self.force_colour || self.colour_depth.supports_colour())
    }

    /// Check if 256-color output is supported
    pub fn supports_256_colours(&self) -> bool {
        self.supports_colour() && self.colour_depth.supports_256_colours()
    }

    /// Check if true color (24-bit) is supported
    pub fn supports_true_colour(&self) -> bool {
        self.supports_colour() && self.colour_depth.supports_true_colour()
    }

    /// Check if advanced formatting (tables, boxes) should use unicode
    pub fn should_use_unicode_boxes(&self) -> bool {
        self.unicode_support && self.supports_colour()
    }
}

/// Detected terminal color scheme (light or dark background)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColourScheme {
    /// Light background (dark text preferred)
    Light,
    /// Dark background (light text preferred)
    #[default]
    Dark,
    /// Unable to detect, assume dark
    Unknown,
}

impl ColourScheme {
    /// Check if this is a light color scheme
    pub fn is_light(self) -> bool {
        matches!(self, ColourScheme::Light)
    }

    /// Check if this is a dark color scheme
    pub fn is_dark(self) -> bool {
        matches!(self, ColourScheme::Dark | ColourScheme::Unknown)
    }

    /// Get a human-readable name
    pub fn name(self) -> &'static str {
        match self {
            ColourScheme::Light => "light",
            ColourScheme::Dark => "dark",
            ColourScheme::Unknown => "unknown",
        }
    }
}

const COLOUR_SCHEME_LIGHT: u8 = 0;
const COLOUR_SCHEME_DARK: u8 = 1;
const COLOUR_SCHEME_UNKNOWN: u8 = 2;
const COLOUR_SCHEME_UNSET: u8 = 255;

static COLOUR_SCHEME_RUNTIME_OVERRIDE: AtomicU8 = AtomicU8::new(COLOUR_SCHEME_UNSET);

/// Detect terminal color scheme from environment.
pub fn detect_colour_scheme() -> ColourScheme {
    if let Some(override_scheme) = colour_scheme_runtime_override() {
        return override_scheme;
    }

    // Check cached value first
    static CACHED: Lazy<ColourScheme> = Lazy::new(detect_colour_scheme_uncached);
    *CACHED
}

fn colour_scheme_runtime_override() -> Option<ColourScheme> {
    match COLOUR_SCHEME_RUNTIME_OVERRIDE.load(Ordering::Relaxed) {
        COLOUR_SCHEME_LIGHT => Some(ColourScheme::Light),
        COLOUR_SCHEME_DARK => Some(ColourScheme::Dark),
        COLOUR_SCHEME_UNKNOWN => Some(ColourScheme::Unknown),
        _ => None,
    }
}

/// Store a runtime color scheme override.
///
/// This is intended to be populated once at startup by terminal OSC probing.
/// Set `None` to clear the runtime override.
pub fn set_colour_scheme_override(value: Option<ColourScheme>) {
    let encoded = match value {
        Some(ColourScheme::Light) => COLOUR_SCHEME_LIGHT,
        Some(ColourScheme::Dark) => COLOUR_SCHEME_DARK,
        Some(ColourScheme::Unknown) => COLOUR_SCHEME_UNKNOWN,
        None => COLOUR_SCHEME_UNSET,
    };
    COLOUR_SCHEME_RUNTIME_OVERRIDE.store(encoded, Ordering::Relaxed);
}

fn detect_colour_scheme_uncached() -> ColourScheme {
    if let Ok(colourfgbg) = std::env::var("COLORFGBG") {
        let parts: Vec<&str> = colourfgbg.split(';').collect();
        if let Some(bg_str) = parts.last()
            && let Ok(bg) = bg_str.parse::<u8>()
        {
            return if bg == 7 || bg == 15 {
                ColourScheme::Light
            } else if bg == 0 || bg == 8 {
                ColourScheme::Dark
            } else if bg > 230 {
                ColourScheme::Light
            } else {
                ColourScheme::Dark
            };
        }
    }

    if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
        let term_lower = term_program.to_lowercase();
        if term_lower.contains("iterm")
            || term_lower.contains("ghostty")
            || term_lower.contains("warp")
            || term_lower.contains("alacritty")
        {
            return ColourScheme::Dark;
        }
    }

    if cfg!(target_os = "macos")
        && let Ok(term_program) = std::env::var("TERM_PROGRAM")
        && term_program == "Apple_Terminal"
    {
        return ColourScheme::Light;
    }

    ColourScheme::Unknown
}

// Cache detection results to avoid repeated system calls
static COLOUR_DEPTH_CACHE: AtomicU8 = AtomicU8::new(255); // 255 = not cached yet

/// Detect the terminal's color depth
fn detect_colour_depth() -> ColourDepth {
    let cached = COLOUR_DEPTH_CACHE.load(Ordering::Relaxed);
    if cached != 255 {
        return match cached {
            0 => ColourDepth::None,
            1 => ColourDepth::Basic16,
            2 => ColourDepth::Colour256,
            3 => ColourDepth::TrueColour,
            _ => ColourDepth::None,
        };
    }

    let depth = if no_colour_env_active() {
        ColourDepth::None
    } else if clicolour_force() {
        ColourDepth::TrueColour
    } else if !clicolour().unwrap_or_else(term_supports_colour) {
        ColourDepth::None
    } else {
        std::env::var("COLORTERM")
            .ok()
            .and_then(|val| {
                let lower = val.to_lowercase();
                if lower.contains("truecolor") || lower.contains("24bit") {
                    Some(ColourDepth::TrueColour)
                } else {
                    None
                }
            })
            .unwrap_or(ColourDepth::Colour256)
    };

    COLOUR_DEPTH_CACHE.store(
        match depth {
            ColourDepth::None => 0,
            ColourDepth::Basic16 => 1,
            ColourDepth::Colour256 => 2,
            ColourDepth::TrueColour => 3,
        },
        Ordering::Relaxed,
    );

    depth
}

/// Detect if unicode is supported by the terminal
fn detect_unicode_support() -> bool {
    std::env::var("LANG")
        .ok()
        .map(|lang| lang.to_lowercase().contains("utf"))
        .or_else(|| std::env::var("LC_ALL").ok().map(|lc| lc.to_lowercase().contains("utf")))
        .unwrap_or(true)
}

/// Global capabilities instance (cached)
pub static CAPABILITIES: Lazy<AnsiCapabilities> = Lazy::new(AnsiCapabilities::detect);

/// Check if NO_COLOR environment variable is set
pub fn is_no_colour() -> bool {
    no_colour_env_active()
}

/// Check if CLICOLOR_FORCE is set
pub fn is_clicolour_force() -> bool {
    clicolour_force()
}
