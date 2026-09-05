//! Shared theme state across the core and TUI facades.

#[test]
fn theme_state_is_shared_across_core_and_tui_wrappers() {
    let original = vtcode_core::ui::theme::active_theme_id();

    vtcode_core::ui::theme::set_active_theme("mono").expect("built-in mono theme must exist");
    assert_eq!(vtcode_ui::tui::ui::theme::active_theme_id(), "mono");

    vtcode_ui::tui::ui::theme::set_active_theme("ansi-classic").expect("built-in ANSI theme must exist");
    assert_eq!(vtcode_core::ui::theme::active_theme_id(), "ansi-classic");

    vtcode_core::ui::theme::set_active_theme(&original).expect("previously active theme must remain registered");
}
