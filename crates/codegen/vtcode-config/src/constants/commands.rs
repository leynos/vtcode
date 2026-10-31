/// Deny regex for recursive `rm` spellings and the long `--force` option, in
/// any flag order or grouping.
///
/// This is the single source for the shipped default rule. The bootstrap
/// template embeds the same text (a raw TOML literal cannot interpolate), and
/// a test asserts the two stay in step.
pub const DEFAULT_RM_DENY_REGEX: &str = r"\brm\s+(\S+\s+)*(-[A-Za-z]*[rR][A-Za-z]*|--recursive|--force)(\s|$)";

pub(crate) const DEFAULT_EXTRA_PATH_ENTRIES: &[&str] = &[
    "$HOME/.cargo/bin",
    "$HOME/.local/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "$HOME/.asdf/bin",
    "$HOME/.asdf/shims",
    "$HOME/go/bin",
];
