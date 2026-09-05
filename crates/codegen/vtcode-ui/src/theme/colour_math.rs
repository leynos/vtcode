use anstyle::RgbColor;
use vtcode_config::constants::ui;

pub(crate) const MAX_DARK_BG_TEXT_LUMINANCE: f32 = 0.92;
pub(crate) const MIN_DARK_BG_TEXT_LUMINANCE: f32 = 0.20;
pub(crate) const MAX_LIGHT_BG_TEXT_LUMINANCE: f32 = 0.68;

pub(crate) fn relative_luminance(colour: RgbColor) -> f32 {
    fn channel(value: u8) -> f32 {
        let c = (value as f32) / 255.0;
        if c <= ui::THEME_RELATIVE_LUMINANCE_CUTOFF {
            c / ui::THEME_RELATIVE_LUMINANCE_LOW_FACTOR
        } else {
            ((c + ui::THEME_RELATIVE_LUMINANCE_OFFSET) / (1.0 + ui::THEME_RELATIVE_LUMINANCE_OFFSET))
                .powf(ui::THEME_RELATIVE_LUMINANCE_EXPONENT)
        }
    }

    let r = channel(colour.0);
    let g = channel(colour.1);
    let b = channel(colour.2);

    ui::THEME_RED_LUMINANCE_COEFFICIENT * r
        + ui::THEME_GREEN_LUMINANCE_COEFFICIENT * g
        + ui::THEME_BLUE_LUMINANCE_COEFFICIENT * b
}

pub(crate) fn contrast_ratio(foreground: RgbColor, background: RgbColor) -> f32 {
    let fg = relative_luminance(foreground);
    let bg = relative_luminance(background);
    let (lighter, darker) = if fg > bg { (fg, bg) } else { (bg, fg) };
    (lighter + ui::THEME_CONTRAST_RATIO_OFFSET) / (darker + ui::THEME_CONTRAST_RATIO_OFFSET)
}

fn darken(colour: RgbColor, ratio: f32) -> RgbColor {
    mix(colour, RgbColor(0, 0, 0), ratio)
}

fn adjust_luminance_to_target(colour: RgbColor, target: f32) -> RgbColor {
    let current = relative_luminance(colour);
    if (current - target).abs() < 1e-3 {
        return colour;
    }

    if current < target {
        let denom = (1.0 - current).max(1e-6);
        let ratio = ((target - current) / denom).clamp(0.0, 1.0);
        lighten(colour, ratio)
    } else {
        let denom = current.max(1e-6);
        let ratio = ((current - target) / denom).clamp(0.0, 1.0);
        darken(colour, ratio)
    }
}

pub(crate) fn balance_text_luminance(colour: RgbColor, background: RgbColor, min_contrast: f32) -> RgbColor {
    let bg_luminance = relative_luminance(background);
    let mut candidate = colour;
    let current = relative_luminance(candidate);
    if bg_luminance < 0.5 {
        if current < MIN_DARK_BG_TEXT_LUMINANCE {
            candidate = adjust_luminance_to_target(candidate, MIN_DARK_BG_TEXT_LUMINANCE);
        } else if current > MAX_DARK_BG_TEXT_LUMINANCE {
            candidate = adjust_luminance_to_target(candidate, MAX_DARK_BG_TEXT_LUMINANCE);
        }
    } else if current > MAX_LIGHT_BG_TEXT_LUMINANCE {
        candidate = adjust_luminance_to_target(candidate, MAX_LIGHT_BG_TEXT_LUMINANCE);
    }

    ensure_contrast(candidate, background, min_contrast, &[colour])
}

pub(crate) fn ensure_contrast(
    candidate: RgbColor,
    background: RgbColor,
    min_ratio: f32,
    fallbacks: &[RgbColor],
) -> RgbColor {
    if contrast_ratio(candidate, background) >= min_ratio {
        return candidate;
    }

    for &fallback in fallbacks {
        if contrast_ratio(fallback, background) >= min_ratio {
            return fallback;
        }
    }

    let black = RgbColor(0, 0, 0);
    let white = RgbColor(255, 255, 255);
    if contrast_ratio(black, background) >= contrast_ratio(white, background) {
        black
    } else {
        white
    }
}

pub(crate) fn mix(colour: RgbColor, target: RgbColor, ratio: f32) -> RgbColor {
    let ratio = ratio.clamp(ui::THEME_MIX_RATIO_MIN, ui::THEME_MIX_RATIO_MAX);
    let blend = |c: u8, t: u8| -> u8 {
        let c = c as f32;
        let t = t as f32;
        ((c + (t - c) * ratio).round()).clamp(ui::THEME_BLEND_CLAMP_MIN, ui::THEME_BLEND_CLAMP_MAX) as u8
    };

    RgbColor(blend(colour.0, target.0), blend(colour.1, target.1), blend(colour.2, target.2))
}

pub(crate) fn lighten(colour: RgbColor, ratio: f32) -> RgbColor {
    mix(
        colour,
        RgbColor(ui::THEME_COLOUR_WHITE_RED, ui::THEME_COLOUR_WHITE_GREEN, ui::THEME_COLOUR_WHITE_BLUE),
        ratio,
    )
}
