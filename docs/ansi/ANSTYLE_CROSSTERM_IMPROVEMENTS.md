# anstyle-crossterm Integration Improvements

## Overview

This document outlines improvements made to VT Code's use of `anstyle-crossterm`, a bridge library that adapts generic `anstyle` styling to `crossterm` (and thus `ratatui` TUI) compatibility.

**Documentation**: https://docs.rs/anstyle-crossterm/latest/anstyle_crossterm/

## Key Improvements

### 1. Current Style Helpers (`crates/codegen/vtcode-ui/src/design/style.rs`)

The internal helper module provides focused conversion and style builders:

#### `fg_bg_style(fg, bg)`

Combines foreground and background colours in a single style.

```rust
use anstyle::{Color, AnsiColor};
use crate::design::style::fg_bg_style;

let style = fg_bg_style(
    Color::Ansi(AnsiColor::Black),
    Color::Ansi(AnsiColor::Yellow),
);
```

#### `bg_style(bg)`

Creates a background style. Apply effects with `Style::add_modifier` when needed.

```rust
use anstyle::{Color, AnsiColor, Effects};
use crate::design::style::bg_style;

let style = bg_style(Color::Ansi(AnsiColor::Blue));
```

#### `with_effects(effects)`

Creates a style with ratatui modifiers derived from `anstyle::Effects`.

```rust
use anstyle::Effects;
use crate::design::style::with_effects;

let style = with_effects(Effects::BOLD | Effects::ITALIC);
```

### 2. Improved Documentation

-   Added comprehensive module-level documentation explaining the anstyle-crossterm adapter pattern
-   Clarified the conversion flow: `anstyle` → `anstyle-crossterm` → `crossterm` → `ratatui`
-   Added examples showing how anstyle-crossterm maps standard colours to indexed variants
-   Documented attribute mapping limitations (some crossterm attributes have no ratatui equivalent)

### 3. Enhanced Attribute Handling

Improved `apply_attributes()` function with:

-   Better inline documentation explaining the mapping
-   Explicit note about unmapped attributes (Hidden, OverLined)
-   Clearer comments on attribute support across the library stack

### 4. Comprehensive Test Coverage

The current helper coverage includes:

```rust
#[test]
fn convenience_fg_bg_style() { /* ... */ }

#[test]
fn convenience_bg_style() { /* ... */ }

#[test]
fn convenience_coloured_with_effects() { /* ... */ }
```

All tests validate:

-   Correct colour mapping through anstyle-crossterm
-   Proper effect application
-   Edge cases (partial styles, no effects, etc.)

## Colour Mapping Behaviour

Due to anstyle-crossterm's design, standard ANSI colours are mapped to indexed variants for terminal compatibility:

| Input Colour| Output (via anstyle-crossterm) |
| ----------- | ------------------------------ |
| Red         | Indexed(52) - DarkRed          |
| Green       | Indexed(22) - DarkGreen        |
| Blue        | Indexed(17) - DarkBlue         |
| Yellow      | Indexed(58) - DarkYellow       |
| Magenta     | Indexed(53) - DarkMagenta      |
| Cyan        | Indexed(23) - DarkCyan         |
| White       | Gray                           |
| BrightBlack | DarkGray                       |

This ensures consistent rendering across different terminal colour schemes.

## Architecture Flow

```

   anstyle Style       Generic styling (CLI-agnostic)
  (Color + Effects)


            anstyle_to_ratatui_style()


 anstyle-crossterm     Conversion library
  to_crossterm()       (handles colour mapping)



 crossterm Style       Terminal capabilities
 (Color + Attrs)       (darker colours, indexed)


            anstyle_to_ratatui_colour()
            + effects_to_modifiers()


  ratatui Style        TUI widget compatible
  (Color + Modifiers)

```

## Usage Patterns

### For CLI Tool Output

Use `AnsiRenderer` with `line_with_style()`:

```rust
use anstyle::Style;
use anstyle::AnsiColor;
use anstyle::Color;

let style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .effects(anstyle::Effects::BOLD);

renderer.line_with_style(style, "styled text")?;
```

### For TUI Components

Convert to ratatui style:

```rust
use crate::design::style::{anstyle_to_ratatui_style, coloured_with_effects};

let anstyle_style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::ITALIC);

let ratatui_style = anstyle_to_ratatui_style(anstyle_style);
// Use with ratatui widgets
```

Or use convenience helpers:

```rust
let style = coloured_with_effects(
    Color::Ansi(AnsiColor::Blue),
    Effects::BOLD | Effects::ITALIC,
);
```

## Testing

All improvements are covered by comprehensive tests:

```bash
cargo nextest run -p vtcode-ui -E 'test(convenience_)'
```

The tests cover colour conversion, effects, and the focused style builders.

## Performance Considerations

-   No runtime overhead: All conversions are synchronous
-   anstyle-crossterm is stateless: No caching or allocation needed
-   Lazy evaluation: Styles are only converted when needed
-   Zero-copy for most operations (except RGB colour components)

## Future Improvements

1. **RGB Colour Support**: Enhance RGB colour handling with optional palette optimization
2. **Theme Integration**: Add theme-aware colour mapping (light/dark mode detection)
3. **Custom Colour Palettes**: Support terminal-specific colour profiles
4. **Attribute Caching**: Cache frequently-used style combinations

## Related Files

-   **Style bridge**: `crates/codegen/vtcode-ui/src/design/style.rs`
- **Colour conversion**: `crates/codegen/vtcode-ui/src/design/colour.rs`
-   **CLI rendering**: `crates/codegen/vtcode-core/src/utils/ansi.rs` (uses `AnsiRenderer`)

## References

-   [anstyle crate](https://docs.rs/anstyle/)
-   [anstyle-crossterm crate](https://docs.rs/anstyle-crossterm/)
-   [crossterm crate](https://docs.rs/crossterm/)
-   [ratatui crate](https://docs.rs/ratatui/)
