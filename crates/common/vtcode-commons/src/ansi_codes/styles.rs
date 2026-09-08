//! ANSI style, cursor, screen, and terminal control constants.

// === Reset ===
/// ANSI escape sequence for `RESET`.
pub const RESET: &str = "\x1b[0m";

// === Text Styles ===
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC: &str = "\x1b[3m";
pub const UNDERLINE: &str = "\x1b[4m";
pub const BLINK: &str = "\x1b[5m";
pub const REVERSE: &str = "\x1b[7m";
pub const HIDDEN: &str = "\x1b[8m";
pub const STRIKETHROUGH: &str = "\x1b[9m";

pub const RESET_BOLD_DIM: &str = "\x1b[22m";
pub const RESET_ITALIC: &str = "\x1b[23m";
pub const RESET_UNDERLINE: &str = "\x1b[24m";
pub const RESET_BLINK: &str = "\x1b[25m";
pub const RESET_REVERSE: &str = "\x1b[27m";
pub const RESET_HIDDEN: &str = "\x1b[28m";
pub const RESET_STRIKETHROUGH: &str = "\x1b[29m";

// === Foreground Colours (30-37) ===
pub const FG_BLACK: &str = "\x1b[30m";
pub const FG_RED: &str = "\x1b[31m";
pub const FG_GREEN: &str = "\x1b[32m";
pub const FG_YELLOW: &str = "\x1b[33m";
pub const FG_BLUE: &str = "\x1b[34m";
pub const FG_MAGENTA: &str = "\x1b[35m";
pub const FG_CYAN: &str = "\x1b[36m";
pub const FG_WHITE: &str = "\x1b[37m";
pub const FG_DEFAULT: &str = "\x1b[39m";

// === Background Colours (40-47) ===
pub const BG_BLACK: &str = "\x1b[40m";
pub const BG_RED: &str = "\x1b[41m";
pub const BG_GREEN: &str = "\x1b[42m";
pub const BG_YELLOW: &str = "\x1b[43m";
pub const BG_BLUE: &str = "\x1b[44m";
pub const BG_MAGENTA: &str = "\x1b[45m";
pub const BG_CYAN: &str = "\x1b[46m";
pub const BG_WHITE: &str = "\x1b[47m";
pub const BG_DEFAULT: &str = "\x1b[49m";

// === Bright Foreground Colours (90-97) ===
pub const FG_BRIGHT_BLACK: &str = "\x1b[90m";
pub const FG_BRIGHT_RED: &str = "\x1b[91m";
pub const FG_BRIGHT_GREEN: &str = "\x1b[92m";
pub const FG_BRIGHT_YELLOW: &str = "\x1b[93m";
pub const FG_BRIGHT_BLUE: &str = "\x1b[94m";
pub const FG_BRIGHT_MAGENTA: &str = "\x1b[95m";
pub const FG_BRIGHT_CYAN: &str = "\x1b[96m";
pub const FG_BRIGHT_WHITE: &str = "\x1b[97m";

// === Bright Background Colours (100-107) ===
pub const BG_BRIGHT_BLACK: &str = "\x1b[100m";
pub const BG_BRIGHT_RED: &str = "\x1b[101m";
pub const BG_BRIGHT_GREEN: &str = "\x1b[102m";
pub const BG_BRIGHT_YELLOW: &str = "\x1b[103m";
pub const BG_BRIGHT_BLUE: &str = "\x1b[104m";
pub const BG_BRIGHT_MAGENTA: &str = "\x1b[105m";
pub const BG_BRIGHT_CYAN: &str = "\x1b[106m";
pub const BG_BRIGHT_WHITE: &str = "\x1b[107m";

// === Cursor Control ===
pub const CURSOR_HOME: &str = "\x1b[H";
pub const CURSOR_HIDE: &str = "\x1b[?25l";
pub const CURSOR_SHOW: &str = "\x1b[?25h";
pub const CURSOR_SAVE_DEC: &str = "\x1b7";
pub const CURSOR_RESTORE_DEC: &str = "\x1b8";
pub const CURSOR_SAVE_SCO: &str = "\x1b[s";
pub const CURSOR_RESTORE_SCO: &str = "\x1b[u";

// === Erase Functions ===
pub const CLEAR_SCREEN: &str = "\x1b[2J";
pub const CLEAR_TO_END_OF_SCREEN: &str = "\x1b[0J";
pub const CLEAR_TO_START_OF_SCREEN: &str = "\x1b[1J";
pub const CLEAR_SAVED_LINES: &str = "\x1b[3J";
pub const CLEAR_LINE: &str = "\x1b[2K";
pub const CLEAR_TO_END_OF_LINE: &str = "\x1b[0K";
pub const CLEAR_TO_START_OF_LINE: &str = "\x1b[1K";

// === Screen Modes ===
pub const ALT_BUFFER_ENABLE: &str = "\x1b[?1049h";
pub const ALT_BUFFER_DISABLE: &str = "\x1b[?1049l";
pub const SCREEN_SAVE: &str = "\x1b[?47h";
pub const SCREEN_RESTORE: &str = "\x1b[?47l";
pub const LINE_WRAP_ENABLE: &str = "\x1b[=7h";
pub const LINE_WRAP_DISABLE: &str = "\x1b[=7l";

// === Scroll Region ===
/// Set Scrolling Region (DECSTBM) — CSI Ps ; Ps r
pub const SCROLL_REGION_RESET: &str = "\x1b[r";

// === Insert / Delete ===
/// Insert Ps Line(s) (default = 1) (IL)
pub const INSERT_LINE: &str = "\x1b[L";
/// Delete Ps Line(s) (default = 1) (DL)
pub const DELETE_LINE: &str = "\x1b[M";
/// Insert Ps Character(s) (default = 1) (ICH)
pub const INSERT_CHAR: &str = "\x1b[@";
/// Delete Ps Character(s) (default = 1) (DCH)
pub const DELETE_CHAR: &str = "\x1b[P";
/// Erase Ps Character(s) (default = 1) (ECH)
pub const ERASE_CHAR: &str = "\x1b[X";

// === Scroll Control ===
/// Scroll up Ps lines (default = 1) (SU)
pub const SCROLL_UP: &str = "\x1b[S";
/// Scroll down Ps lines (default = 1) (SD)
pub const SCROLL_DOWN: &str = "\x1b[T";

// === ESC-level Controls (C1 equivalents) ===
/// Index — move cursor down one line, scroll if at bottom (IND)
pub const INDEX: &str = "\x1bD";
/// Next Line — move to first position of next line (NEL)
pub const NEXT_LINE: &str = "\x1bE";
/// Horizontal Tab Set (HTS)
pub const TAB_SET: &str = "\x1bH";
/// Reverse Index — move cursor up one line, scroll if at top (RI)
pub const REVERSE_INDEX: &str = "\x1bM";
/// Full Reset (RIS) — reset terminal to initial state
pub const FULL_RESET: &str = "\x1bc";
/// Application Keypad (DECPAM)
pub const KEYPAD_APPLICATION: &str = "\x1b=";
/// Normal Keypad (DECPNM)
pub const KEYPAD_NUMERIC: &str = "\x1b>";

// === Mouse Tracking Modes (DECSET/DECRST) ===
/// X10 mouse reporting — button press only (mode 9)
pub const MOUSE_X10_ENABLE: &str = "\x1b[?9h";
pub const MOUSE_X10_DISABLE: &str = "\x1b[?9l";
/// Normal mouse tracking — press and release (mode 1000)
pub const MOUSE_NORMAL_ENABLE: &str = "\x1b[?1000h";
pub const MOUSE_NORMAL_DISABLE: &str = "\x1b[?1000l";
/// Button-event mouse tracking (mode 1002)
pub const MOUSE_BUTTON_EVENT_ENABLE: &str = "\x1b[?1002h";
pub const MOUSE_BUTTON_EVENT_DISABLE: &str = "\x1b[?1002l";
/// Any-event mouse tracking (mode 1003)
pub const MOUSE_ANY_EVENT_ENABLE: &str = "\x1b[?1003h";
pub const MOUSE_ANY_EVENT_DISABLE: &str = "\x1b[?1003l";
/// SGR extended mouse coordinates (mode 1006)
pub const MOUSE_SGR_ENABLE: &str = "\x1b[?1006h";
pub const MOUSE_SGR_DISABLE: &str = "\x1b[?1006l";
/// URXVT extended mouse coordinates (mode 1015)
pub const MOUSE_URXVT_ENABLE: &str = "\x1b[?1015h";
pub const MOUSE_URXVT_DISABLE: &str = "\x1b[?1015l";

// === Terminal Mode Controls (DECSET/DECRST) ===
/// Bracketed Paste Mode (mode 2004)
pub const BRACKETED_PASTE_ENABLE: &str = "\x1b[?2004h";
pub const BRACKETED_PASTE_DISABLE: &str = "\x1b[?2004l";
/// Focus Event Tracking (mode 1004)
pub const FOCUS_EVENT_ENABLE: &str = "\x1b[?1004h";
pub const FOCUS_EVENT_DISABLE: &str = "\x1b[?1004l";
/// Synchronized Output (mode 2026) — batch rendering
pub const SYNC_OUTPUT_BEGIN: &str = "\x1b[?2026h";
pub const SYNC_OUTPUT_END: &str = "\x1b[?2026l";
/// Application Cursor Keys (DECCKM, mode 1)
pub const APP_CURSOR_KEYS_ENABLE: &str = "\x1b[?1h";
pub const APP_CURSOR_KEYS_DISABLE: &str = "\x1b[?1l";
/// Origin Mode (DECOM, mode 6)
pub const ORIGIN_MODE_ENABLE: &str = "\x1b[?6h";
pub const ORIGIN_MODE_DISABLE: &str = "\x1b[?6l";
/// Auto-Wrap Mode (DECAWM, mode 7)
pub const AUTO_WRAP_ENABLE: &str = "\x1b[?7h";
pub const AUTO_WRAP_DISABLE: &str = "\x1b[?7l";

// === Device Status / Attributes ===
/// Primary Device Attributes (DA1) — request
pub const DEVICE_ATTRIBUTES_REQUEST: &str = "\x1b[c";
/// Device Status Report — request cursor position (DSR CPR)
pub const CURSOR_POSITION_REQUEST: &str = "\x1b[6n";
/// Device Status Report — request terminal status
pub const DEVICE_STATUS_REQUEST: &str = "\x1b[5n";

// === OSC Sequences (Operating System Commands) ===
/// Set window title — OSC 2 ; Pt BEL
pub const OSC_SET_TITLE_PREFIX: &str = "\x1b]2;";
/// Set icon name — OSC 1 ; Pt BEL
pub const OSC_SET_ICON_PREFIX: &str = "\x1b]1;";
/// Set icon name and title — OSC 0 ; Pt BEL
pub const OSC_SET_ICON_AND_TITLE_PREFIX: &str = "\x1b]0;";
/// Query/set foreground colour — OSC 10
pub const OSC_FG_COLOUR_PREFIX: &str = "\x1b]10;";
/// Query/set background colour — OSC 11
pub const OSC_BG_COLOUR_PREFIX: &str = "\x1b]11;";
/// Query/set cursor colour — OSC 12
pub const OSC_CURSOR_COLOUR_PREFIX: &str = "\x1b]12;";
/// Clipboard access — OSC 52
pub const OSC_CLIPBOARD_PREFIX: &str = "\x1b]52;";

// === Character Set Designation (ISO 2022) ===
/// Select UTF-8 character set
pub const CHARSET_UTF8: &str = "\x1b%G";
/// Select default (ISO 8859-1) character set
pub const CHARSET_DEFAULT: &str = "\x1b%@";
