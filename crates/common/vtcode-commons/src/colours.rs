//! Colour utilities for VT Code
//!
//! This module provides colour manipulation capabilities using anstyle,
//! which offers low-level ANSI styling with RGB and 256-colour support.

#![expect(
    clippy::cast_possible_truncation,
    reason = "RGB interpolation is clamped to the byte range before conversion."
)]

use anstyle::{AnsiColor, Color, Effects, RgbColor, Style};

/// Create an RGB colour from hex string
pub fn colour_from_hex(hex: &str) -> Option<Color> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }

    let mut components = hex.as_bytes().chunks_exact(2);
    let r = u8::from_str_radix(std::str::from_utf8(components.next()?).ok()?, 16).ok()?;
    let g = u8::from_str_radix(std::str::from_utf8(components.next()?).ok()?, 16).ok()?;
    let b = u8::from_str_radix(std::str::from_utf8(components.next()?).ok()?, 16).ok()?;

    Some(Color::Rgb(RgbColor(r, g, b)))
}

/// Blend two RGB colours
#[allow(
    clippy::cast_sign_loss,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub fn blend_colours(colour1: &Color, colour2: &Color, ratio: f32) -> Option<Color> {
    let rgb1 = colour_to_rgb(colour1)?;
    let rgb2 = colour_to_rgb(colour2)?;

    let r = (rgb1.r() as f32 * (1.0 - ratio) + rgb2.r() as f32 * ratio) as u8;
    let g = (rgb1.g() as f32 * (1.0 - ratio) + rgb2.g() as f32 * ratio) as u8;
    let b = (rgb1.b() as f32 * (1.0 - ratio) + rgb2.b() as f32 * ratio) as u8;

    Some(Color::Rgb(RgbColor(r, g, b)))
}

/// Convert an ANSI colour to RGB, if possible
fn colour_to_rgb(colour: &Color) -> Option<RgbColor> {
    match colour {
        Color::Rgb(rgb) => Some(*rgb),
        Color::Ansi(ansi_colour) => ansi_to_rgb(*ansi_colour),
        Color::Ansi256(ansi256_colour) => ansi256_to_rgb(*ansi256_colour),
    }
}

/// Convert an ANSI colour to RGB approximation
fn ansi_to_rgb(ansi_colour: AnsiColor) -> Option<RgbColor> {
    match ansi_colour {
        AnsiColor::Black => Some(RgbColor(0, 0, 0)),
        AnsiColor::Red => Some(RgbColor(170, 0, 0)),
        AnsiColor::Green => Some(RgbColor(0, 170, 0)),
        AnsiColor::Yellow => Some(RgbColor(170, 85, 0)),
        AnsiColor::Blue => Some(RgbColor(0, 0, 170)),
        AnsiColor::Magenta => Some(RgbColor(170, 0, 170)),
        AnsiColor::Cyan => Some(RgbColor(0, 170, 170)),
        AnsiColor::White => Some(RgbColor(170, 170, 170)),
        AnsiColor::BrightBlack => Some(RgbColor(85, 85, 85)),
        AnsiColor::BrightRed => Some(RgbColor(255, 85, 85)),
        AnsiColor::BrightGreen => Some(RgbColor(85, 255, 85)),
        AnsiColor::BrightYellow => Some(RgbColor(255, 255, 85)),
        AnsiColor::BrightBlue => Some(RgbColor(85, 85, 255)),
        AnsiColor::BrightMagenta => Some(RgbColor(255, 85, 255)),
        AnsiColor::BrightCyan => Some(RgbColor(85, 255, 255)),
        AnsiColor::BrightWhite => Some(RgbColor(255, 255, 255)),
    }
}

/// Convert an ANSI256 colour to RGB approximation
fn ansi256_to_rgb(ansi256_colour: anstyle::Ansi256Color) -> Option<RgbColor> {
    let code = ansi256_colour.0;
    match code {
        0 => Some(RgbColor(0, 0, 0)),
        1 => Some(RgbColor(170, 0, 0)),
        2 => Some(RgbColor(0, 170, 0)),
        3 => Some(RgbColor(170, 85, 0)),
        4 => Some(RgbColor(0, 0, 170)),
        5 => Some(RgbColor(170, 0, 170)),
        6 => Some(RgbColor(0, 170, 170)),
        7 => Some(RgbColor(170, 170, 170)),
        8 => Some(RgbColor(85, 85, 85)),
        9 => Some(RgbColor(255, 85, 85)),
        10 => Some(RgbColor(85, 255, 85)),
        11 => Some(RgbColor(255, 255, 85)),
        12 => Some(RgbColor(85, 85, 255)),
        13 => Some(RgbColor(255, 85, 255)),
        14 => Some(RgbColor(85, 255, 255)),
        15 => Some(RgbColor(255, 255, 255)),
        n if (16..=231).contains(&n) => {
            let adjusted = n - 16;
            let r = adjusted / 36;
            let g = (adjusted % 36) / 6;
            let b = adjusted % 6;
            let scale = |x: u8| -> u8 { if x == 0 { 0 } else { 55 + x * 40 } };
            Some(RgbColor(scale(r), scale(g), scale(b)))
        }
        n if n >= 232 => {
            let gray = 8 + (n - 232) * 10;
            Some(RgbColor(gray, gray, gray))
        }
        _ => Some(RgbColor(128, 128, 128)),
    }
}

/// Determine if a colour is light (for contrast calculations)
pub fn is_light_colour(colour: &Color) -> bool {
    let rgb = colour_to_rgb(colour);
    if let Some(RgbColor(r, g, b)) = rgb {
        let luminance = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0;
        luminance > 0.5
    } else {
        false
    }
}

/// Get a contrasting colour (black or white) for better readability
pub fn contrasting_colour(colour: &Color) -> Color {
    if is_light_colour(colour) {
        Color::Ansi(AnsiColor::Black)
    } else {
        Color::Ansi(AnsiColor::White)
    }
}

/// Create a desaturated version of a colour
#[allow(
    clippy::cast_sign_loss,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub fn desaturate_colour(colour: &Color, amount: f32) -> Option<Color> {
    let rgb = colour_to_rgb(colour)?;
    let r = rgb.r() as f32;
    let g = rgb.g() as f32;
    let b = rgb.b() as f32;
    let gray = 0.299 * r + 0.587 * g + 0.114 * b;
    let r_new = r * (1.0 - amount) + gray * amount;
    let g_new = g * (1.0 - amount) + gray * amount;
    let b_new = b * (1.0 - amount) + gray * amount;
    Some(Color::Rgb(RgbColor(r_new as u8, g_new as u8, b_new as u8)))
}

fn styled(text: &str, style: Style) -> String {
    format!("{}{}{}", style.render(), text, style.render_reset())
}

/// Style wrapper for console::style compatibility
pub fn style(text: impl std::fmt::Display) -> StyledString {
    StyledString { text: text.to_string(), style: Style::new() }
}

pub struct StyledString {
    text: String,
    style: Style,
}

impl StyledString {
    pub fn red(mut self) -> Self {
        self.style = self.style.fg_color(Some(Color::Ansi(AnsiColor::Red)));
        self
    }

    pub fn green(mut self) -> Self {
        self.style = self.style.fg_color(Some(Color::Ansi(AnsiColor::Green)));
        self
    }

    pub fn blue(mut self) -> Self {
        self.style = self.style.fg_color(Some(Color::Ansi(AnsiColor::Blue)));
        self
    }

    pub fn yellow(mut self) -> Self {
        self.style = self.style.fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
        self
    }

    pub fn cyan(mut self) -> Self {
        self.style = self.style.fg_color(Some(Color::Ansi(AnsiColor::Cyan)));
        self
    }

    pub fn magenta(mut self) -> Self {
        self.style = self.style.fg_color(Some(Color::Ansi(AnsiColor::Magenta)));
        self
    }

    pub fn bold(mut self) -> Self {
        self.style = self.style.effects(self.style.get_effects() | Effects::BOLD);
        self
    }

    fn dimmed(mut self) -> Self {
        self.style = self.style.effects(self.style.get_effects() | Effects::DIMMED);
        self
    }

    pub fn dim(self) -> Self {
        self.dimmed()
    }

    pub fn on_black(mut self) -> Self {
        self.style = self.style.bg_color(Some(Color::Ansi(AnsiColor::Black)));
        self
    }
}

impl std::fmt::Display for StyledString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}{}", self.style.render(), self.text, self.style.render_reset())
    }
}

/// Apply red colour to text
pub fn red(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))))
}

/// Apply green colour to text
pub fn green(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
}

/// Apply blue colour to text
pub fn blue(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Blue))))
}

/// Apply yellow colour to text
pub fn yellow(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow))))
}

/// Apply purple colour to text
pub fn purple(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta))))
}

/// Apply cyan colour to text
pub fn cyan(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
}

/// Apply white colour to text
pub fn white(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::White))))
}

/// Apply black colour to text
pub fn black(text: &str) -> String {
    styled(text, Style::new().fg_color(Some(Color::Ansi(AnsiColor::Black))))
}

/// Apply bold styling to text
pub fn bold(text: &str) -> String {
    styled(text, Style::new().effects(Effects::BOLD))
}

/// Apply italic styling to text
pub fn italic(text: &str) -> String {
    styled(text, Style::new().effects(Effects::ITALIC))
}

/// Apply underline styling to text
pub fn underline(text: &str) -> String {
    styled(text, Style::new().effects(Effects::UNDERLINE))
}

/// Apply dimmed styling to text
pub fn dimmed(text: &str) -> String {
    styled(text, Style::new().effects(Effects::DIMMED))
}

/// Apply blinking styling to text
pub fn blink(text: &str) -> String {
    styled(text, Style::new().effects(Effects::BLINK))
}

/// Apply reversed styling to text
pub fn reversed(text: &str) -> String {
    styled(text, Style::new().effects(Effects::INVERT))
}

/// Apply strikethrough styling to text
pub fn strikethrough(text: &str) -> String {
    styled(text, Style::new().effects(Effects::STRIKETHROUGH))
}

/// Apply custom RGB colour to text
pub fn rgb(text: &str, r: u8, g: u8, b: u8) -> String {
    styled(text, Style::new().fg_color(Some(Color::Rgb(RgbColor(r, g, b)))))
}

/// Combine multiple colour and style operations
pub fn custom_style(text: &str, styles: &[&str]) -> String {
    let mut style = Style::new();

    for style_str in styles {
        match *style_str {
            "red" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Red))),
            "green" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Green))),
            "blue" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Blue))),
            "yellow" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
            "purple" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Magenta))),
            "cyan" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Cyan))),
            "white" => style = style.fg_color(Some(Color::Ansi(AnsiColor::White))),
            "black" => style = style.fg_color(Some(Color::Ansi(AnsiColor::Black))),
            "bold" => style = style.effects(style.get_effects() | Effects::BOLD),
            "italic" => style = style.effects(style.get_effects() | Effects::ITALIC),
            "underline" => style = style.effects(style.get_effects() | Effects::UNDERLINE),
            "dimmed" => style = style.effects(style.get_effects() | Effects::DIMMED),
            "blink" => style = style.effects(style.get_effects() | Effects::BLINK),
            "reversed" => style = style.effects(style.get_effects() | Effects::INVERT),
            "strikethrough" => style = style.effects(style.get_effects() | Effects::STRIKETHROUGH),
            _ => {}
        }
    }

    styled(text, style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_hex_is_rejected_without_panicking() {
        assert!(colour_from_hex("红色").is_none());
    }
}
