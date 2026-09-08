use super::*;
use ratatui::crossterm::event::KeyModifiers;
use std::sync::Arc;
use std::time::Instant;

use super::super::types::{
    ContentPart, DiffPreviewMode, InlineTextStyle, TransientEvent, TransientSelectionChange, TransientSubmission,
};
use crate::tui::core_tui::app::session::transient::TransientSurface;
use crate::tui::core_tui::app::types::InlineMessageKind;
use crate::tui::core_tui::runner::TuiSessionDriver;
use crate::tui::core_tui::session::action::{Action, is_readline_editing_key, normalize_terminal_control_event};
use crate::tui::core_tui::session::clipboard_image::{ClipboardImageError, read_clipboard_image};
use crate::tui::core_tui::session::modal::{ModalKeyModifiers, ModalListKeyResult};
use crate::tui::core_tui::session::mode_switch_guard::{self};
use crate::tui::core_tui::session::reverse_search;
use crate::tui::core_tui::style::theme_from_styles;
use crate::tui::core_tui::types::InlineSegment;
use crate::tui::core_tui::types::{
    InlineEvent as CoreInlineEvent, OverlayEvent, OverlaySelectionChange, SubmittedInput,
};
use crate::tui::ui::theme;

fn input_history_entries(session: &Session) -> Vec<(String, Vec<ContentPart>, chrono::DateTime<chrono::Utc>)> {
    session
        .core
        .input_manager
        .history()
        .iter()
        .map(|entry| (entry.content().to_string(), entry.attachment_elements(), entry.timestamp()))
        .collect()
}

pub(super) fn handle_paste(session: &mut Session, content: &str) -> Option<InlineEvent> {
    // Secure prompt modal: auto-submit pasted content directly without requiring Enter.
    // This saves the API key to .env immediately and dismisses the modal.
    if let Some(modal) = session.modal_state_mut()
        && modal.secure_prompt.is_some()
        && modal.list.is_none()
    {
        let submitted = content.trim().to_string();
        if submitted.is_empty() {
            return None;
        }
        // Close the modal immediately so the UI does not show a stale overlay
        // while the event is in-flight to the interaction loop.
        session.close_overlay();
        session.mark_dirty();
        return Some(InlineEvent::Submit(submitted.into()));
    }

    if let Some(viewer) = session.tool_output_viewer_state_mut()
        && viewer.search_active()
    {
        viewer.insert_search_text(content);
        session.mark_dirty();
    } else if session.core.input_enabled() {
        session.insert_paste_text(content);
        session.update_input_triggers();
        session.mark_dirty();
    } else if session.history_picker_visible() {
        let history = input_history_entries(session);
        session.history_picker_state.search_query.push_str(content);
        session.history_picker_state.update_search(&history);
        session.mark_dirty();
    } else if let Some(modal) = session.modal_state_mut()
        && let (Some(list), Some(search)) = (modal.list.as_mut(), modal.search.as_mut())
    {
        search.insert(content);
        list.apply_search(&search.query);
        session.mark_dirty();
    } else if let Some(wizard) = session.wizard_overlay_mut()
        && let Some(search) = wizard.search.as_mut()
    {
        search.insert(content);
        if let Some(step) = wizard.steps.get_mut(wizard.current_step) {
            step.list.apply_search(&search.query);
        }
        session.mark_dirty();
    }
    None
}

fn copy_selected_input_if_requested(session: &mut Session, key: &KeyEvent, has_command: bool) -> bool {
    if has_command && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
        if session.core.copy_input_selection_to_clipboard() {
            session.mark_dirty();
        }
        return true;
    }

    let is_copy_shortcut = match key.code {
        KeyCode::Char('c') | KeyCode::Char('C') => key.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::Char('\u{3}') => true,
        _ => false,
    };

    if !is_copy_shortcut {
        return false;
    }

    if session.core.copy_input_selection_to_clipboard() {
        session.mark_dirty();
        return true;
    }

    false
}

fn image_paste_warning(error: ClipboardImageError) -> &'static str {
    match error {
        ClipboardImageError::NoImage => "No image found in clipboard.",
        ClipboardImageError::ClipboardUnavailable => {
            "Clipboard image paste is unavailable in this terminal or desktop session."
        }
        ClipboardImageError::UnsupportedModel => "The selected model does not support image input.",
        ClipboardImageError::WslFallbackFailure => "Could not read a clipboard image from Windows via PowerShell.",
    }
}

fn push_warning_line(session: &mut Session, text: &'static str) {
    session.push_line(
        InlineMessageKind::Warning,
        vec![InlineSegment {
            text: text.to_string(),
            style: Arc::new(InlineTextStyle::default()),
        }],
    );
    session.core.request_transcript_clear();
    session.mark_dirty();
}

fn is_image_paste_shortcut(key: &KeyEvent, has_control: bool, has_alt: bool, has_command: bool) -> bool {
    matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V'))
        && !has_command
        && ((has_control && !has_alt) || (has_alt && !has_control))
}

fn handle_image_paste_shortcut_with(
    session: &mut Session,
    mut image_reader: impl FnMut() -> Result<ContentPart, ClipboardImageError>,
) {
    if !session.core.image_input_enabled() {
        push_warning_line(session, image_paste_warning(ClipboardImageError::UnsupportedModel));
        return;
    }

    match image_reader() {
        Ok(image) => {
            if let Some(attachment_number) = session.core.input_manager.push_attachment(image) {
                session.core.input_manager.insert_text(&format!("[Image #{attachment_number}]"));
            }
            session
                .core
                .set_input_compact_mode(session.core.input_compact_placeholder().is_some());
            session.update_input_triggers();
            session.mark_dirty();
        }
        Err(error) => push_warning_line(session, image_paste_warning(error)),
    }
}

pub(super) fn process_key(session: &mut Session, key: KeyEvent) -> Option<InlineEvent> {
    process_key_with_clipboard_image_reader(session, key, read_clipboard_image)
}

/// Key handler for a secure-prompt modal (text-only, masked input such as an
/// API-key entry). See `process_key_with_clipboard_image_reader` for why this
/// exists separately from the normal composer handler.
///
/// Every key is handled here — editing keys mutate the shared input manager,
/// Enter submits and closes the modal, Esc cancels and closes it, and anything
/// else is consumed so no composer shortcut leaks through while a secret is
/// being typed.
fn handle_secure_prompt_key(
    session: &mut Session,
    key: KeyEvent,
    has_control: bool,
    has_shift: bool,
    has_alt: bool,
    has_command: bool,
) -> Option<InlineEvent> {
    match key.code {
        KeyCode::Esc => {
            session.core.input_manager.clear();
            session.close_overlay();
            session.mark_dirty();
            None
        }
        // Plain Enter and Cmd+Enter submit. Shift/Ctrl+Alt line-feed combos are
        // not meaningful for a single-line secret, so require no shift/alt/control.
        KeyCode::Enter if !has_control && !has_shift && !has_alt => {
            let submitted = session.core.input_manager.content().trim().to_string();
            if submitted.is_empty() {
                return None;
            }
            session.core.input_manager.clear();
            session.close_overlay();
            session.mark_dirty();
            Some(InlineEvent::Submit(submitted.into()))
        }
        KeyCode::Backspace => {
            if has_alt {
                session.delete_word_backward();
            } else if has_command {
                session.delete_to_start_of_line();
            } else {
                session.delete_char();
            }
            session.mark_dirty();
            None
        }
        KeyCode::Delete => {
            if has_command {
                session.delete_to_end_of_line();
            } else {
                session.delete_char_forward();
            }
            session.mark_dirty();
            None
        }
        KeyCode::Left => {
            if has_command {
                session.move_to_start();
            } else if has_alt {
                session.move_left_word();
            } else {
                session.move_left();
            }
            session.mark_dirty();
            None
        }
        KeyCode::Right => {
            if has_command {
                session.move_to_end();
            } else if has_alt {
                session.move_right_word();
            } else {
                session.move_right();
            }
            session.mark_dirty();
            None
        }
        KeyCode::Home => {
            session.move_to_start();
            session.mark_dirty();
            None
        }
        KeyCode::End => {
            session.move_to_end();
            session.mark_dirty();
            None
        }
        KeyCode::Char(ch) => {
            // Readline-style Ctrl+<ch> editing/navigation (no Alt/Cmd).
            if has_control && !has_alt && !has_command {
                match ch {
                    'a' | 'A' => {
                        session.move_to_start();
                        session.mark_dirty();
                    }
                    'e' | 'E' => {
                        session.move_to_end();
                        session.mark_dirty();
                    }
                    'b' | 'B' => {
                        session.move_left();
                        session.mark_dirty();
                    }
                    'f' | 'F' => {
                        session.move_right();
                        session.mark_dirty();
                    }
                    'w' | 'W' => {
                        session.delete_word_backward();
                        session.mark_dirty();
                    }
                    'u' | 'U' => {
                        session.delete_to_start_of_line();
                        session.mark_dirty();
                    }
                    'k' | 'K' => {
                        session.delete_to_end_of_line();
                        session.mark_dirty();
                    }
                    'h' | 'H' => {
                        // Ctrl+H is backspace on many terminals.
                        session.delete_char();
                        session.mark_dirty();
                    }
                    _ => {}
                }
                return None;
            }
            // Plain character insertion (Shift is allowed — produces the shifted
            // glyph). Control characters (Tab, newline, etc.) are ignored so the
            // field stays a single-line secret.
            if !has_control && !has_alt && !has_command && !ch.is_control() {
                session.insert_char(ch);
                session.mark_dirty();
            }
            None
        }
        // Consume everything else (function keys, Tab, modifiers, …) so the
        // secure prompt stays focused and no composer shortcut fires.
        _ => None,
    }
}

pub(super) fn process_key_with_clipboard_image_reader(
    session: &mut Session,
    key: KeyEvent,
    image_reader: impl FnMut() -> Result<ContentPart, ClipboardImageError>,
) -> Option<InlineEvent> {
    let key = normalize_terminal_control_event(key);
    let modifiers = key.modifiers;
    let has_control = modifiers.contains(KeyModifiers::CONTROL);
    let has_shift = modifiers.contains(KeyModifiers::SHIFT);
    let raw_alt = modifiers.contains(KeyModifiers::ALT);
    let raw_meta = modifiers.contains(KeyModifiers::META);
    let has_super = modifiers.contains(KeyModifiers::SUPER);
    // Command key detection: prioritize Command/Super over Alt
    // On macOS: Command = SUPER, on some terminals Alt = META
    let has_command = has_super || raw_meta;
    let has_alt = raw_alt && !has_command;

    if copy_selected_input_if_requested(session, &key, has_command) {
        return None;
    }

    if is_image_paste_shortcut(&key, has_control, has_alt, has_command) {
        if session.core.input_enabled() {
            handle_image_paste_shortcut_with(session, image_reader);
        }
        return None;
    }

    // Secure prompt modals own their own key handling. They render as
    // `FloatingOverlay` (Modal focus policy), which disables the regular
    // composer via `core.input_enabled() == false`. The normal key handler
    // gates every character insertion on `input_enabled()`, so without this
    // dedicated handler typed characters would never reach the masked input
    // field. The handler edits the shared input manager directly — the modal
    // renderer reads `input_manager.content()` / `cursor()` to display the
    // masked value — and scopes accepted keys to text editing plus
    // submit/cancel, so composer shortcuts (Ctrl+M, Alt+S, …) do not leak
    // through while the user is entering a secret.
    if session
        .modal_state_mut()
        .is_some_and(|m| m.list.is_none() && m.secure_prompt.is_some())
    {
        return handle_secure_prompt_key(session, key, has_control, has_shift, has_alt, has_command);
    }

    if let Some(modal) = session.modal_state_mut() {
        let modal_modifiers = ModalKeyModifiers {
            control: has_control,
            alt: has_alt,
            command: has_command,
        };

        if let Some(action) = modal.hotkey_action(&key, modal_modifiers) {
            session.close_overlay();
            session.mark_dirty();
            return Some(InlineEvent::Transient(TransientEvent::Submitted(TransientSubmission::Hotkey(action.into()))));
        }

        // Text-only modals (no list, no secure prompt): close on Esc or any
        // keypress. Secure prompt modals are handled above and excluded here
        // so character input can flow through to the normal input handler.
        if modal.list.is_none() && modal.secure_prompt.is_none() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    session.close_overlay();
                    session.mark_dirty();
                    return None;
                }
                _ => {
                    // Consume all other key events so they don't reach the input handler
                    return None;
                }
            }
        }

        let result = modal.handle_list_key_event(&key, modal_modifiers);

        match result {
            ModalListKeyResult::Redraw => {
                session.mark_dirty();
                return None;
            }
            ModalListKeyResult::Emit(event) => {
                session.mark_dirty();
                // Synchronous preview: fire the callback and sync session theme
                // before returning so the render picks up the preview in the
                // same frame as the cursor movement.
                if let Some(ref cb) = session.preview_callback
                    && let CoreInlineEvent::Overlay(OverlayEvent::SelectionChanged(OverlaySelectionChange::List(
                        ref selection,
                    ))) = event
                {
                    let _ = cb(Some(selection));
                    if theme::has_preview_theme() {
                        let styles = theme::active_styles();
                        let inline_theme = theme_from_styles(&styles);
                        session.core.theme = inline_theme;
                        session.core.styles.set_theme(session.core.theme.clone());
                    }
                }
                return Some(event.into());
            }
            ModalListKeyResult::HandledNoRedraw => {
                return None;
            }
            ModalListKeyResult::Submit(event) | ModalListKeyResult::Cancel(event) => {
                session.close_overlay();
                return Some(event.into());
            }
            ModalListKeyResult::NotHandled => {}
        }
    }

    let configured_action = session.core.resolve_rebindable_action(&key);
    match configured_action {
        Some(Action::ToggleToolDisplayMode) => {
            session.invalidate_transcript_cache();
            session.mark_dirty();
            return Some(InlineEvent::ToggleToolDisplayMode);
        }
        Some(Action::ToggleTaskPanel) => {
            session.toggle_task_panel();
            return None;
        }
        Some(Action::OpenTranscriptReview) => {
            let width = session.core.transcript_width.max(1);
            let height = session.core.transcript_rows.max(1);
            if session.tool_output_viewer_state().is_some() {
                session.close_tool_output_viewer();
            } else {
                session.open_tool_output_viewer(width, height, None);
            }
            return None;
        }
        Some(Action::ToggleTranscriptRenderMode) => {
            if let Some(viewer) = session.tool_output_viewer_state_mut() {
                viewer.toggle_render_mode();
                session.mark_dirty();
                return None;
            }
        }
        _ => {}
    }

    if let Some(wizard) = session.wizard_overlay_mut() {
        let result = wizard.handle_key_event(
            &key,
            ModalKeyModifiers {
                control: has_control,
                alt: has_alt,
                command: has_command,
            },
        );

        match result {
            ModalListKeyResult::Redraw => {
                session.mark_dirty();
                return None;
            }
            ModalListKeyResult::Emit(event) => {
                session.mark_dirty();
                return Some(event.into());
            }
            ModalListKeyResult::HandledNoRedraw => {
                return None;
            }
            ModalListKeyResult::Submit(event) => {
                session.close_overlay();
                return Some(event.into());
            }
            ModalListKeyResult::Cancel(event) => {
                session.close_overlay();
                return Some(event.into());
            }
            ModalListKeyResult::NotHandled => {}
        }
    }

    match session.handle_local_agents_key(&key) {
        local_agents::LocalAgentsKeyResult::Emit(event) => return Some(event),
        local_agents::LocalAgentsKeyResult::Handled => return None,
        local_agents::LocalAgentsKeyResult::NotHandled => {}
    }

    if session.inline_lists_visible() && session.handle_agent_palette_key(&key) {
        return None;
    }

    if session.inline_lists_visible() && session.handle_file_palette_key(&key) {
        return None;
    }

    if slash::try_handle_slash_navigation(session, &key, has_control, has_alt, has_command) {
        return None;
    }

    match handle_tool_output_viewer_key(session, &key, has_control, has_alt, has_command) {
        ToolOutputViewerKeyResult::Emit(event) => return Some(event),
        ToolOutputViewerKeyResult::Handled => return None,
        ToolOutputViewerKeyResult::NotHandled => {}
    }

    if let Some(event) = handle_diff_preview_key(session, &key) {
        return Some(event);
    }

    // Handle history picker (Ctrl+R) - Visual fuzzy search for command history
    if has_control && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) && !session.history_picker_visible() {
        open_history_picker(session);
        return None;
    }

    // Handle forward search (Ctrl+S) - Readline forward search
    if has_control && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) && !session.history_picker_visible() {
        open_history_picker(session);
        return None;
    }

    // Handle history picker if active
    if session.inline_lists_visible() && session.history_picker_visible() {
        let history = input_history_entries(session);
        let was_active = session.history_picker_visible();
        let handled = history_picker::handle_history_picker_key(
            &key,
            &mut session.history_picker_state,
            &mut session.core.input_manager,
            &history,
        );
        if handled {
            session.finish_history_picker_interaction(was_active);
            session.mark_dirty();
            return None;
        }
    }

    if session.handle_vim_key(&key) {
        return None;
    }

    if is_inline_lists_toggle_shortcut(&key, has_control, has_alt, has_command) {
        session.toggle_inline_lists_visibility();
        return None;
    }

    // Legacy reverse search handling (kept for backward compatibility)
    // Handle reverse search (Ctrl+R) - disabled in favor of history picker
    // if has_control && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
    //     if !session.core.reverse_search_state.active {
    //         session.core.reverse_search_state.start_search(
    //             &session.core.input_manager,
    //             &session.core.input_manager.history_texts(),
    //         );
    //         session.mark_dirty();
    //         return None;
    //     }
    // }

    // Handle reverse search if active (legacy)
    if session.core.reverse_search_state.active {
        // Get history first to avoid borrow conflicts
        let history = session.core.input_manager.history_texts();
        let handled = reverse_search::handle_reverse_search_key(
            &key,
            &mut session.core.reverse_search_state,
            &mut session.core.input_manager,
            &history,
        );
        if handled {
            session.mark_dirty();
            return None;
        }
    }

    if let Some(action) = configured_action {
        let contextual_arrow_action = match key.code {
            KeyCode::Up => Some(Action::HistoryPrevious),
            KeyCode::Down => Some(Action::HistoryNext),
            _ => None,
        };
        let preserve_contextual_arrow = contextual_arrow_action == Some(action);
        let overridden_contextual_arrow = contextual_arrow_action
            .is_some_and(|contextual_action| session.core.rebindable_action_is_overridden(contextual_action));

        let render_mode_without_viewer =
            action == Action::ToggleTranscriptRenderMode && session.tool_output_viewer_state().is_none();
        if !render_mode_without_viewer && !is_readline_editing_key(&key) && !preserve_contextual_arrow {
            return session.core.dispatch_rebindable_action(action).map(Into::into);
        }

        if overridden_contextual_arrow && contextual_arrow_action != Some(action) {
            return None;
        }
    } else if let Some(contextual_action) = match key.code {
        KeyCode::Up => Some(Action::HistoryPrevious),
        KeyCode::Down => Some(Action::HistoryNext),
        _ => None,
    } && session.core.rebindable_action_is_overridden(contextual_action)
    {
        // An explicit replacement or empty list removes the old arrow action;
        // do not let the legacy contextual fallback resurrect it.
        return None;
    }

    match key.code {
        KeyCode::Char('c') | KeyCode::Char('C') if has_control => {
            if session.core.mouse_selection.has_selection {
                session.core.mouse_selection.request_copy();
                session.mark_dirty();
                return None;
            }
            let now = Instant::now();
            if session
                .core
                .last_interrupt_press
                .is_some_and(|last| now.duration_since(last).as_millis() < 1_000)
            {
                session.core.last_interrupt_press = None;
                session.request_exit();
                session.mark_dirty();
                return Some(InlineEvent::Exit);
            }
            session.core.last_interrupt_press = Some(now);
            if session.has_active_overlay() {
                session.close_overlay();
            }
            session.mark_dirty();
            Some(InlineEvent::Interrupt)
        }
        KeyCode::Char('\u{3}') => {
            if session.core.mouse_selection.has_selection {
                session.core.mouse_selection.request_copy();
                session.mark_dirty();
                return None;
            }
            let now = Instant::now();
            if session
                .core
                .last_interrupt_press
                .is_some_and(|last| now.duration_since(last).as_millis() < 1_000)
            {
                session.core.last_interrupt_press = None;
                session.request_exit();
                session.mark_dirty();
                return Some(InlineEvent::Exit);
            }
            session.core.last_interrupt_press = Some(now);
            if session.has_active_overlay() {
                session.close_overlay();
            }
            session.mark_dirty();
            Some(InlineEvent::Interrupt)
        }
        KeyCode::Char('d') if has_control => {
            session.mark_dirty();
            Some(InlineEvent::Exit)
        }
        KeyCode::Char('b') if has_control && !has_alt && !has_command => {
            // Ctrl+B: Move back a character (Readline) - overrides background operation
            if session.core.input_enabled() {
                session.move_left();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('f') if has_control && !has_alt && !has_command => {
            // Ctrl+F: Move forward a character (Readline)
            if session.core.input_enabled() {
                session.move_right();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('p') if has_control && !has_alt && !has_command => {
            // Ctrl+P: Fetch the previous command from history (Readline)
            if session.navigate_history_previous() {
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('n') if has_control && !has_alt && !has_command => {
            // Ctrl+N: Fetch the next command from history (Readline)
            if session.navigate_history_next() {
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('\u{14}')
            if (has_control || matches!(key.code, KeyCode::Char('\u{14}'))) && !has_alt && !has_command =>
        {
            // The app-level review action handles its configured binding before
            // this fallback. Ctrl+T reaches here only when review is explicitly
            // unbound, preserving the original Readline transpose behaviour.
            if session.core.input_enabled() {
                session.transpose_chars();
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('m') | KeyCode::Char('M') if has_control && !has_alt && !has_command => {
            session.mark_dirty();
            Some(InlineEvent::Submit("/model".into()))
        }
        KeyCode::Char('s') | KeyCode::Char('S') if has_alt && !has_control && !has_command => {
            session.mark_dirty();
            Some(InlineEvent::Submit("/subprocesses".into()))
        }
        KeyCode::Char('a') | KeyCode::Char('A') if has_control && !has_command && !has_alt => {
            if session.core.input_enabled() {
                session.move_to_start();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('g') | KeyCode::Char('G')
            if has_control && !has_command && !has_alt && session.core.input_enabled() =>
        {
            let draft = session.core.input_manager.content().to_string();
            session.mark_dirty();
            Some(InlineEvent::LaunchEditor { draft })
        }
        KeyCode::Char('w') | KeyCode::Char('W') if has_control && !has_command && !has_alt => {
            if session.core.input_enabled() {
                session.delete_word_backward();
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('u') | KeyCode::Char('U') if has_control && !has_command && !has_alt => {
            if session.core.input_enabled() {
                session.delete_to_start_of_line();
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('k') | KeyCode::Char('K') if has_control && !has_command && !has_alt => {
            if session.core.input_enabled() {
                session.delete_to_end_of_line();
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('j') if has_control => {
            // Ctrl+J is a line feed character, insert newline for multiline input
            session.insert_char('\n');
            session.mark_dirty();
            None
        }
        KeyCode::Char('z') | KeyCode::Char('Z')
            if has_control && !has_command && !has_alt && session.core.input_enabled() =>
        {
            session.core.input_manager.undo();
            session.mark_dirty();
            None
        }
        KeyCode::Char('y') | KeyCode::Char('Y')
            if has_control && !has_command && !has_alt && session.core.input_enabled() =>
        {
            session.core.input_manager.redo();
            session.mark_dirty();
            None
        }
        KeyCode::Char('l') | KeyCode::Char('L') if has_control => {
            session.mark_dirty();
            Some(InlineEvent::Submit("/clear".into()))
        }
        KeyCode::BackTab => {
            session.clear_inline_prompt_suggestion();
            session.mark_dirty();
            if session.is_running_activity() {
                push_mode_switch_busy_notice(session);
                return None;
            }
            Some(InlineEvent::CyclePrimaryAgentPrevious)
        }
        KeyCode::Esc => {
            if session.has_active_overlay() {
                session.core.last_esc_press = None;
                session.close_overlay();
                None
            } else if session.is_running_activity() || session.active_pty_session_count() > 0 {
                session.core.last_esc_press = None;
                session.mark_dirty();
                Some(InlineEvent::Interrupt)
            } else if !session.core.input_manager.content().is_empty() {
                // Escape with content: clear input
                session.core.last_esc_press = None;
                session
                    .core
                    .handle_command(crate::tui::core_tui::types::InlineCommand::ClearInput);
                session.mark_dirty();
                None
            } else {
                // Escape with no content: detect double-Esc for rewind
                let now = Instant::now();
                let is_double_esc = session
                    .core
                    .last_esc_press
                    .is_some_and(|last| now.duration_since(last).as_millis() < 500);
                if is_double_esc {
                    session.core.last_esc_press = None;
                    session.mark_dirty();
                    Some(InlineEvent::Submit("/rewind".into()))
                } else {
                    session.core.last_esc_press = Some(now);
                    session.mark_dirty();
                    Some(InlineEvent::Cancel)
                }
            }
        }
        KeyCode::PageUp => {
            session.scroll_page_up();
            session.mark_dirty();
            Some(InlineEvent::ScrollPageUp)
        }
        KeyCode::PageDown => {
            session.scroll_page_down();
            session.mark_dirty();
            Some(InlineEvent::ScrollPageDown)
        }
        KeyCode::Home if has_control && session.core.fullscreen.active => {
            session.scroll_to_top();
            session.mark_dirty();
            None
        }
        KeyCode::End if has_control && session.core.fullscreen.active => {
            session.scroll_to_bottom();
            session.mark_dirty();
            None
        }
        KeyCode::Up => {
            let edit_queue_modifier = has_alt || (raw_meta && !has_super);
            if !crate::tui::core_tui::session::terminal_capabilities::queued_input_edit_uses_shift_left()
                && edit_queue_modifier
                && !session.core.queued_inputs.is_empty()
            {
                if let Some(latest) = session.pop_latest_queued_input() {
                    session.clear_inline_prompt_suggestion();
                    session.core.input_manager.set_content(latest);
                    session
                        .core
                        .set_input_compact_mode(session.input_compact_placeholder().is_some());
                    session.core.scroll_manager.set_offset(0);
                    slash::update_slash_suggestions(session);
                }
                session.mark_dirty();
                Some(InlineEvent::EditQueue)
            } else if session.navigate_history_previous() {
                session.mark_dirty();
                Some(InlineEvent::HistoryPrevious)
            } else {
                None
            }
        }
        KeyCode::Down => {
            if session.should_open_local_agents_with_down(&key, has_control, has_alt, has_command) {
                session.open_local_agents_drawer(false);
                session.mark_dirty();
                return None;
            }
            if session.navigate_history_next() {
                session.clear_inline_prompt_suggestion();
                session.mark_dirty();
                Some(InlineEvent::HistoryNext)
            } else {
                None
            }
        }
        KeyCode::Enter => {
            if !session.core.input_enabled() {
                return None;
            }

            if session.file_palette_visible() {
                if let Some(palette) = session.file_palette.as_ref()
                    && !palette.get_selected().is_some_and(|e| e.is_dir)
                    && let Some(entry) = palette.get_selected()
                {
                    let file_path = entry.relative_path.clone();
                    session.insert_file_reference(&file_path);
                    session.close_file_palette();
                    session.mark_dirty();
                    return Some(InlineEvent::FileSelected(file_path));
                }
                return None;
            }

            if maybe_show_help_modal(session) {
                return None;
            }

            if !has_control && let Some(event) = maybe_handle_busy_steering_command(session) {
                return Some(event);
            }

            if !has_control && handle_running_slash_command_block(session) {
                return None;
            }

            if !has_control
                && !has_shift
                && !has_alt
                && session.core.input_manager.content().trim().is_empty()
                && session.active_pty_session_count() > 0
            {
                session.mark_dirty();
                return Some(InlineEvent::Submit("/jobs".into()));
            }

            // Check for backslash + Enter quick escape (insert newline without submitting)
            if !has_control && session.core.input_manager.content().ends_with('\\') {
                // Remove the backslash and insert a newline
                let mut content = session.core.input_manager.content().to_string();
                content.pop(); // Remove the backslash
                content.push('\n');
                session.core.input_manager.set_content(content);
                session.mark_dirty();
                return None;
            }

            if has_control {
                let Some(submitted) = take_submitted_input(session) else {
                    session.mark_dirty();
                    return if session.is_running_activity() {
                        None
                    } else {
                        Some(InlineEvent::ProcessLatestQueued)
                    };
                };
                session.mark_dirty();

                return if session.is_running_activity() {
                    match extract_slash_command_name(&submitted.text) {
                        Some("stop") => Some(InlineEvent::Interrupt),
                        Some("pause") => Some(InlineEvent::Pause),
                        Some("resume") => Some(InlineEvent::Resume),
                        other => {
                            // Match the reference agents: Ctrl+Enter while a
                            // turn is running joins the visible queue, so the
                            // message hangs above the composer, renders as the
                            // user's own bubble when dispatched, and can be
                            // edited via Shift+← before it sends. Plain messages
                            // are marked batchable so several queued messages
                            // coalesce into ONE turn (batching applies to plain
                            // Ctrl+Enter only); slash commands and plain Enter
                            // stay one per turn so command intent is preserved.
                            if let Some(command_name) = other {
                                tracing::debug!(target: "vtcode_ui::keys", %command_name, "ctrl+enter queued slash command");
                                session.push_queued_input(submitted.text.clone());
                                Some(InlineEvent::QueueSubmit(submitted))
                            } else {
                                tracing::debug!(target: "vtcode_ui::keys", "ctrl+enter queued message");
                                session.push_queued_input(submitted.text.clone());
                                Some(InlineEvent::QueueSubmit(submitted.batchable()))
                            }
                        }
                    }
                } else {
                    Some(InlineEvent::Submit(submitted))
                };
            }

            // Check for multiline input options (Shift/Alt)
            if has_shift || has_alt {
                // Insert newline for multiline input
                session.insert_char('\n');
                session.mark_dirty();
                return None;
            }

            let should_submit_now = slash::should_submit_immediately_from_palette(session);
            let Some(submitted) = take_submitted_input(session) else {
                session.mark_dirty();
                return None;
            };

            session.mark_dirty();

            if should_submit_now {
                return Some(InlineEvent::Submit(submitted));
            }

            // If a turn is actively running, queue the message so it starts immediately after
            // the current turn completes. Otherwise submit directly so the turn starts now.
            if session.is_running_activity() {
                session.push_queued_input(submitted.text.clone());
                Some(InlineEvent::QueueSubmit(submitted))
            } else {
                Some(InlineEvent::Submit(submitted))
            }
        }
        KeyCode::Tab => {
            if !session.core.input_enabled() {
                return None;
            }

            if session.accept_inline_prompt_suggestion() {
                session.update_input_triggers();
                return None;
            }

            if mode_switch_guard::try_cycle_primary_agent(session, &key) {
                session.mark_dirty();
                return Some(InlineEvent::CyclePrimaryAgent);
            }
            None
        }
        KeyCode::Backspace => {
            if session.core.input_enabled() {
                if has_alt {
                    session.delete_word_backward();
                } else if has_command {
                    session.delete_to_start_of_line();
                } else {
                    session.delete_char();
                }
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Delete => {
            if session.core.input_enabled() {
                if has_alt {
                    session.delete_word_backward();
                } else if has_command {
                    session.delete_to_end_of_line();
                } else {
                    session.delete_char_forward();
                }
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        KeyCode::Left => {
            if session.core.input_enabled() {
                let tmux_queue_edit = has_shift
                    && !has_control
                    && !has_command
                    && !has_alt
                    && crate::tui::core_tui::session::terminal_capabilities::queued_input_edit_uses_shift_left()
                    && !session.core.queued_inputs.is_empty();
                if tmux_queue_edit {
                    if let Some(latest) = session.pop_latest_queued_input() {
                        session.clear_inline_prompt_suggestion();
                        session.core.input_manager.set_content(latest);
                        session
                            .core
                            .set_input_compact_mode(session.input_compact_placeholder().is_some());
                        session.core.scroll_manager.set_offset(0);
                        slash::update_slash_suggestions(session);
                    }
                    session.mark_dirty();
                    return Some(InlineEvent::EditQueue);
                }

                session.clear_inline_prompt_suggestion();
                if has_shift && has_command {
                    session.select_to_start();
                } else if has_shift {
                    session.select_left();
                } else if has_command {
                    session.move_to_start();
                } else if has_alt {
                    session.move_left_word();
                } else {
                    session.move_left();
                }
                session.mark_dirty();
            }
            None
        }
        KeyCode::Right => {
            if session.core.input_enabled() {
                session.clear_inline_prompt_suggestion();
                if has_shift && has_command {
                    session.select_to_end();
                } else if has_shift {
                    session.select_right();
                } else if has_command {
                    session.move_to_end();
                } else if has_alt {
                    session.move_right_word();
                } else {
                    session.move_right();
                }
                session.mark_dirty();
            }
            None // Right arrow never triggers any event, including editor launch
        }
        KeyCode::Home => {
            if session.core.input_enabled() {
                session.clear_inline_prompt_suggestion();
                if has_shift {
                    session.select_to_start();
                } else {
                    session.move_to_start();
                }
                session.mark_dirty();
            }
            None
        }
        KeyCode::End => {
            if session.core.input_enabled() {
                session.clear_inline_prompt_suggestion();
                if has_shift {
                    session.select_to_end();
                } else {
                    session.move_to_end();
                }
                session.mark_dirty();
            }
            None
        }
        KeyCode::Char('o') | KeyCode::Char('O') if has_control && !has_alt && !has_command => {
            // Ctrl+O: Copy last agent response as markdown to clipboard
            session.mark_dirty();
            Some(InlineEvent::Submit("/copy".into()))
        }
        KeyCode::Char(ch) => {
            if !session.core.input_enabled() {
                return None;
            }

            if has_alt && matches!(ch, 'p' | 'P') {
                session.clear_inline_prompt_suggestion();
                session.mark_dirty();
                return Some(InlineEvent::RequestInlinePromptSuggestion(
                    session.core.input_manager.content().to_string(),
                ));
            }

            if ch == '?' && !has_control && !has_alt && !has_command && session.core.input_manager.content().is_empty()
            {
                session.show_help_modal();
                return None;
            }

            if ch == '\t' {
                if session.accept_inline_prompt_suggestion() {
                    session.update_input_triggers();
                    return None;
                }
                if mode_switch_guard::try_cycle_primary_agent(session, &key) {
                    session.mark_dirty();
                    return Some(InlineEvent::CyclePrimaryAgent);
                }
                return None;
            }

            if has_command {
                match ch {
                    'a' | 'A' => {
                        session.move_to_start();
                        session.mark_dirty();
                        return None;
                    }
                    'e' | 'E' => {
                        session.move_to_end();
                        session.mark_dirty();
                        return None;
                    }
                    _ => {}
                }
            }

            if has_alt {
                match ch {
                    'b' | 'B' => {
                        session.move_left_word();
                        session.mark_dirty();
                    }
                    'f' | 'F' => {
                        session.move_right_word();
                        session.mark_dirty();
                    }
                    // Alt+D: Kill (cut) forwards to the end of the current word
                    'd' | 'D' if session.core.input_enabled() => {
                        session.delete_word_forward();
                        session.update_input_triggers();
                        session.mark_dirty();
                    }
                    // Alt+U: Uppercase the current word
                    'u' | 'U' if session.core.input_enabled() => {
                        session.uppercase_word();
                        session.update_input_triggers();
                        session.mark_dirty();
                    }
                    // Alt+L: Lowercase the current word
                    'l' | 'L' if session.core.input_enabled() => {
                        session.lowercase_word();
                        session.update_input_triggers();
                        session.mark_dirty();
                    }
                    // Alt+C: Capitalize the current word
                    'c' | 'C' if session.core.input_enabled() => {
                        session.capitalize_word();
                        session.update_input_triggers();
                        session.mark_dirty();
                    }
                    // Alt+\: Delete whitespace around the cursor
                    '\\' if session.core.input_enabled() => {
                        session.delete_whitespace_around_cursor();
                        session.update_input_triggers();
                        session.mark_dirty();
                    }
                    _ => {}
                }
                return None;
            }

            if has_control {
                match ch {
                    'f' | 'F' => {
                        // Ctrl+F: Move forward a character (Readline)
                        if session.core.input_enabled() {
                            session.move_right();
                            session.mark_dirty();
                        }
                        return None;
                    }
                    'b' | 'B' => {
                        // Ctrl+B: Move back a character (Readline)
                        if session.core.input_enabled() {
                            session.move_left();
                            session.mark_dirty();
                        }
                        return None;
                    }
                    'p' | 'P' => {
                        // Ctrl+P: Fetch the previous command from history (Readline)
                        if session.navigate_history_previous() {
                            session.mark_dirty();
                        }
                        return None;
                    }
                    'n' | 'N' => {
                        // Ctrl+N: Fetch the next command from history (Readline)
                        if session.navigate_history_next() {
                            session.mark_dirty();
                        }
                        return None;
                    }
                    't' | 'T' => {
                        // Ctrl+T: Transpose characters (Readline)
                        if session.core.input_enabled() {
                            session.transpose_chars();
                            session.update_input_triggers();
                            session.mark_dirty();
                        }
                        return None;
                    }
                    _ => {}
                }
            }

            if !has_control {
                session.insert_char(ch);
                session.update_input_triggers();
                session.mark_dirty();
            }
            None
        }
        _ => None,
    }
}

pub(super) fn open_history_picker(session: &mut Session) {
    if session.history_picker_state.active {
        return;
    }

    session.ensure_inline_lists_visible_for_trigger();
    session.show_transient_surface(TransientSurface::HistoryPicker);
    session.history_picker_state.open(&session.core.input_manager);
    let history = input_history_entries(session);
    session.history_picker_state.update_search(&history);
    session.mark_dirty();
}

fn is_inline_lists_toggle_shortcut(key: &KeyEvent, has_control: bool, has_alt: bool, has_command: bool) -> bool {
    if !has_control || has_alt || has_command {
        return false;
    }

    matches!(
        key.code,
        KeyCode::Char('i') | KeyCode::Char('I') | KeyCode::Char('/') | KeyCode::Char('?') | KeyCode::Char('\u{1f}')
    )
}

fn maybe_show_help_modal(session: &mut Session) -> bool {
    if session.core.input_manager.content().trim() != "/help" {
        return false;
    }

    clear_submitted_input(session);
    session.show_help_modal();
    true
}

enum ToolOutputViewerKeyResult {
    NotHandled,
    Handled,
    Emit(InlineEvent),
}

fn handle_tool_output_viewer_key(
    session: &mut Session,
    key: &KeyEvent,
    has_control: bool,
    has_alt: bool,
    has_command: bool,
) -> ToolOutputViewerKeyResult {
    let toggle_shortcut = has_control
        && !has_alt
        && !has_command
        && session
            .core
            .resolve_rebindable_action(key)
            .is_some_and(|action| action == Action::OpenTranscriptReview);
    let compatibility_alias =
        has_alt && !has_control && !has_command && matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'));
    if session.tool_output_viewer_state().is_none() {
        if !toggle_shortcut && !compatibility_alias {
            return ToolOutputViewerKeyResult::NotHandled;
        }

        let width = session.core.transcript_width.max(1);
        let height = session.core.transcript_rows.max(1);
        session.open_tool_output_viewer(width, height, None);
        return ToolOutputViewerKeyResult::Handled;
    }

    let complete_copy_shortcut =
        has_control && !has_alt && !has_command && matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O'));
    if complete_copy_shortcut {
        let text = session
            .tool_output_viewer_state_mut()
            .map(|viewer| viewer.export_text())
            .unwrap_or_default();
        session.core.copy_text_to_clipboard(&text);
        session.mark_dirty();
        return ToolOutputViewerKeyResult::Handled;
    }

    if toggle_shortcut {
        session.close_tool_output_viewer();
        return ToolOutputViewerKeyResult::Handled;
    }

    let viewer_copy_shortcut =
        matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{3}')) && has_control;
    if viewer_copy_shortcut && session.core.mouse_selection.has_selection {
        session.core.mouse_selection.request_copy();
        session.mark_dirty();
        return ToolOutputViewerKeyResult::Handled;
    }

    let fallback_height = session.core.transcript_rows.max(1);
    let Some(viewer) = session.tool_output_viewer_state_mut() else {
        return ToolOutputViewerKeyResult::Handled;
    };
    let viewport_height = viewer.content_height_or(fallback_height);

    if viewer.search_active() {
        match key.code {
            KeyCode::Esc => {
                viewer.cancel_search();
                session.mark_dirty();
                return ToolOutputViewerKeyResult::Handled;
            }
            KeyCode::Enter => {
                viewer.commit_search(viewport_height);
                session.mark_dirty();
                return ToolOutputViewerKeyResult::Handled;
            }
            KeyCode::Backspace => {
                viewer.backspace_search();
                session.mark_dirty();
                return ToolOutputViewerKeyResult::Handled;
            }
            KeyCode::Char(ch) if !has_control && !has_alt && !has_command => {
                viewer.insert_search_text(&ch.to_string());
                session.mark_dirty();
                return ToolOutputViewerKeyResult::Handled;
            }
            _ => {
                return ToolOutputViewerKeyResult::Handled;
            }
        }
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
            session.close_tool_output_viewer();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('/') if !has_control && !has_alt && !has_command => {
            viewer.start_search();
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('n') if !has_control && !has_alt && !has_command => {
            viewer.jump_next_match(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('N') if !has_control && !has_alt && !has_command => {
            viewer.jump_previous_match(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Up | KeyCode::Char('k') if !has_control && !has_alt && !has_command => {
            viewer.scroll_line_up(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Down | KeyCode::Char('j') if !has_control && !has_alt && !has_command => {
            viewer.scroll_line_down(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::PageUp => {
            viewer.scroll_half_page_up(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::PageDown => {
            viewer.scroll_half_page_down(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('u') | KeyCode::Char('U') if has_control && !has_alt && !has_command => {
            viewer.scroll_half_page_up(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('d') | KeyCode::Char('D') if has_control && !has_alt && !has_command => {
            viewer.scroll_half_page_down(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('b') | KeyCode::Char('B') if !has_alt && !has_command => {
            viewer.scroll_full_page_up(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('f') | KeyCode::Char('F') if has_control && !has_alt && !has_command => {
            viewer.scroll_full_page_down(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char(' ') if !has_control && !has_alt && !has_command => {
            viewer.scroll_full_page_down(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Home | KeyCode::Char('g') if !has_control && !has_alt && !has_command => {
            viewer.scroll_to_top();
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::End | KeyCode::Char('G') if !has_control && !has_alt && !has_command => {
            viewer.scroll_to_bottom(viewport_height);
            session.mark_dirty();
            ToolOutputViewerKeyResult::Handled
        }
        KeyCode::Char('[') if !has_control && !has_alt && !has_command => {
            ToolOutputViewerKeyResult::Emit(InlineEvent::OpenToolOutputScrollback(viewer.export_text()))
        }
        KeyCode::Char('v') | KeyCode::Char('V') if !has_control && !has_alt && !has_command => {
            ToolOutputViewerKeyResult::Emit(InlineEvent::OpenToolOutputInEditor(viewer.export_text()))
        }
        _ => ToolOutputViewerKeyResult::Handled,
    }
}

fn can_cycle_primary_agent(session: &Session, key: &KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE
        && session.visible_transient_surface().is_none()
        && !session.has_active_overlay()
}

/// Notice shown when the user requests a mode switch (primary-agent cycle or
/// planning workflow) while a turn is actively processing. Mode switches are
/// locked for the duration of a turn to keep agent state consistent.
fn push_mode_switch_busy_notice(session: &mut Session) {
    session.push_line(
        InlineMessageKind::Warning,
        vec![InlineSegment {
            text: mode_switch_guard::MODE_SWITCH_BUSY_NOTICE.to_string(),
            style: Arc::new(InlineTextStyle::default()),
        }],
    );
    session.core.request_transcript_clear();
    session.mark_dirty();
}

impl mode_switch_guard::ModeSwitchGuardSession for Session {
    fn is_running_activity(&self) -> bool {
        TuiSessionDriver::is_running_activity(self)
    }

    fn can_cycle_primary_agent(&self, key: &KeyEvent) -> bool {
        can_cycle_primary_agent(self, key)
    }

    fn notify_mode_switch_busy(&mut self) {
        push_mode_switch_busy_notice(self);
    }
}

fn take_submitted_input(session: &mut Session) -> Option<SubmittedInput> {
    let submitted = session.core.input_manager.content().to_owned();
    let submitted_entry = session.core.input_manager.current_history_entry();
    clear_submitted_input(session);

    if submitted_entry.is_empty() {
        return None;
    }

    let attachments = submitted_entry.attachment_elements();
    session.remember_submitted_input(submitted_entry);
    Some(SubmittedInput::new(submitted, attachments))
}

fn clear_submitted_input(session: &mut Session) {
    session.core.input_manager.clear();
    session.clear_suggested_prompt_state();
    session.clear_inline_prompt_suggestion();
    session.core.set_input_compact_mode(false);
    session.core.scroll_manager.set_offset(0);
    session.update_input_triggers();
}

fn handle_running_slash_command_block(session: &mut Session) -> bool {
    let input = session.core.input_manager.content().to_owned();
    handle_running_slash_command_block_for_input(session, &input)
}

fn handle_running_slash_command_block_for_input(session: &mut Session, input: &str) -> bool {
    if !session.is_running_activity() {
        return false;
    }

    let Some(command_name) = extract_slash_command_name(input) else {
        return false;
    };

    // Read-only local commands are safe to defer: falling through lets the normal
    // queueing path run them right after the current turn instead of dropping them.
    if matches!(command_name, "copy") {
        return false;
    }

    // Mode switches (agent selection, planning workflow) are locked while a turn
    // is processing; surface the dedicated notice for those commands.
    let message = if matches!(command_name, "mode" | "plan") {
        mode_switch_guard::MODE_SWITCH_BUSY_NOTICE.to_string()
    } else {
        format!(
            "'/{command_name}' is disabled while a task is in progress. Please wait for the current task to complete before using this command."
        )
    };
    session.push_line(
        InlineMessageKind::Warning,
        vec![InlineSegment {
            text: message,
            style: Arc::new(InlineTextStyle::default()),
        }],
    );
    session.core.request_transcript_clear();
    session.mark_dirty();
    true
}

fn maybe_handle_busy_steering_command(session: &mut Session) -> Option<InlineEvent> {
    if !TuiSessionDriver::is_running_activity(session) {
        return None;
    }

    let event = match extract_slash_command_name(session.core.input_manager.content()) {
        Some("stop") => InlineEvent::Interrupt,
        Some("pause") => InlineEvent::Pause,
        Some("resume") => InlineEvent::Resume,
        _ => return None,
    };

    clear_submitted_input(session);
    session.mark_dirty();
    Some(event)
}

fn extract_slash_command_name(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    let command_input = trimmed.strip_prefix('/')?;
    let command = command_input.split_whitespace().next()?;
    if command.is_empty() { None } else { Some(command) }
}

/// Emits an InlineEvent through the event channel and callback
#[inline]
pub(super) fn emit_inline_event(
    event: &InlineEvent,
    events: &UnboundedSender<InlineEvent>,
    callback: Option<&(dyn Fn(&InlineEvent) + Send + Sync + 'static)>,
) {
    if let Some(cb) = callback {
        cb(event);
    }
    let _ = events.send(event.clone());
}

/// Handles scroll down event from mouse input
#[inline]
fn handle_scroll_down(
    session: &mut Session,
    events: &UnboundedSender<InlineEvent>,
    callback: Option<&(dyn Fn(&InlineEvent) + Send + Sync + 'static)>,
) {
    session.scroll_line_down();
    session.mark_dirty();
    emit_inline_event(&InlineEvent::ScrollLineDown, events, callback);
}

/// Handles scroll up event from mouse input
#[inline]
fn handle_scroll_up(
    session: &mut Session,
    events: &UnboundedSender<InlineEvent>,
    callback: Option<&(dyn Fn(&InlineEvent) + Send + Sync + 'static)>,
) {
    session.scroll_line_up();
    session.mark_dirty();
    emit_inline_event(&InlineEvent::ScrollLineUp, events, callback);
}

fn handle_diff_preview_key(session: &mut Session, key: &KeyEvent) -> Option<InlineEvent> {
    let mode = session.diff_preview_state()?.mode;

    match key.code {
        KeyCode::Tab => {
            let diff_state = session.diff_preview_state_mut()?;
            if diff_state.current_hunk + 1 < diff_state.hunk_count() {
                diff_state.current_hunk += 1;
            }
            session.mark_dirty();
            None
        }
        KeyCode::BackTab => {
            let diff_state = session.diff_preview_state_mut()?;
            if diff_state.current_hunk > 0 {
                diff_state.current_hunk -= 1;
            }
            session.mark_dirty();
            None
        }
        KeyCode::Enter => {
            session.close_diff_overlay();
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::Submitted(match mode {
                DiffPreviewMode::EditApproval => TransientSubmission::DiffApply,
                DiffPreviewMode::FileConflict => TransientSubmission::DiffProceed,
                DiffPreviewMode::ReadonlyReview => TransientSubmission::DiffAbort,
            })))
        }
        KeyCode::Char('r') | KeyCode::Char('R') if matches!(mode, DiffPreviewMode::FileConflict) => {
            session.close_diff_overlay();
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::Submitted(TransientSubmission::DiffReload)))
        }
        KeyCode::Esc => {
            session.close_diff_overlay();
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::Submitted(match mode {
                DiffPreviewMode::EditApproval => TransientSubmission::DiffReject,
                DiffPreviewMode::FileConflict => TransientSubmission::DiffAbort,
                DiffPreviewMode::ReadonlyReview => TransientSubmission::DiffAbort,
            })))
        }
        KeyCode::Char('1') if matches!(mode, DiffPreviewMode::EditApproval) => {
            let diff_state = session.diff_preview_state_mut()?;
            diff_state.trust_mode = crate::tui::core_tui::app::types::TrustMode::Once;
            let mode = diff_state.trust_mode;
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::SelectionChanged(TransientSelectionChange::DiffTrustMode {
                mode,
            })))
        }
        KeyCode::Char('2') if matches!(mode, DiffPreviewMode::EditApproval) => {
            let diff_state = session.diff_preview_state_mut()?;
            diff_state.trust_mode = crate::tui::core_tui::app::types::TrustMode::Session;
            let mode = diff_state.trust_mode;
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::SelectionChanged(TransientSelectionChange::DiffTrustMode {
                mode,
            })))
        }
        KeyCode::Char('3') if matches!(mode, DiffPreviewMode::EditApproval) => {
            let diff_state = session.diff_preview_state_mut()?;
            diff_state.trust_mode = crate::tui::core_tui::app::types::TrustMode::Always;
            let mode = diff_state.trust_mode;
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::SelectionChanged(TransientSelectionChange::DiffTrustMode {
                mode,
            })))
        }
        KeyCode::Char('4') if matches!(mode, DiffPreviewMode::EditApproval) => {
            let diff_state = session.diff_preview_state_mut()?;
            diff_state.trust_mode = crate::tui::core_tui::app::types::TrustMode::AutoTrust;
            let mode = diff_state.trust_mode;
            session.mark_dirty();
            Some(InlineEvent::Transient(TransientEvent::SelectionChanged(TransientSelectionChange::DiffTrustMode {
                mode,
            })))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::core_tui::app::types::{
        CompactActivityMetadata, InlineCommand, ModalOverlayRequest, TransientRequest,
    };
    use crate::tui::core_tui::session::action::BindingStore;
    use crate::tui::core_tui::types::{
        InlineMessageKind, InlineSegment, InlineTextStyle, InlineTheme, SecurePromptConfig,
    };
    use hashbrown::HashMap;
    use ratatui::Terminal;
    use std::sync::Arc;

    fn build_session() -> Session {
        let mut session = Session::new(InlineTheme::default(), None, 24);
        session.core.set_fullscreen_active(true);
        session.core.apply_transcript_rows(8);
        session.core.apply_transcript_width(60);
        session
    }

    fn text_segment(text: impl Into<String>) -> InlineSegment {
        InlineSegment {
            text: text.into(),
            style: Arc::new(InlineTextStyle::default()),
        }
    }

    fn rendered_buffer_text(terminal: &Terminal<ratatui::backend::TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|column| buffer.cell((column, row)).expect("buffer cell").symbol())
                    .collect::<Vec<_>>()
                    .concat()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn add_compact_activity(session: &mut Session, id: u64, command: &str) {
        session.handle_command(InlineCommand::RecordToolOutput {
            id,
            lines: vec![format!("• Ran {command}"), "  └ complete output".to_string()],
        });
        session.handle_command(InlineCommand::AppendCompactActivity(CompactActivityMetadata {
            group_id: id,
            command_count: 1,
            command: Some(command.to_string().into()),
            hidden_line_count: 1,
            suffix: None,
            review_anchor: Some(id),
            review_anchors: vec![id],
        }));
    }

    #[test]
    fn ctrl_o_emits_copy_command() {
        let mut session = build_session();
        session
            .core
            .push_line(InlineMessageKind::Agent, vec![text_segment("agent reply")]);

        let event = session.process_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(
            matches!(event, Some(InlineEvent::Submit(ref cmd)) if cmd == "/copy"),
            "Ctrl+O should emit Submit(\"/copy\"), got {event:?}"
        );
    }

    #[test]
    fn ctrl_t_opens_and_closes_tool_output_viewer_in_fullscreen() {
        let mut session = build_session();
        session.tool_output_blocks.push(ToolOutputBlock {
            lines: vec!["• Ran echo hello".to_string(), "  └ hello".to_string()],
            ..Default::default()
        });
        session.tool_output_revision = 1;

        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(session.process_key(key).is_none());
        assert!(session.tool_output_viewer_state().is_some());

        assert!(session.process_key(key).is_none());
        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn ctrl_t_opens_tool_output_viewer_outside_fullscreen() {
        let mut session = Session::new(InlineTheme::default(), None, 24);
        session.core.input_manager.set_content("abc".to_string());
        session.core.input_manager.set_cursor(1);

        let result = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));

        assert!(result.is_none());
        assert_eq!(session.core.input_manager.content(), "abc");
        assert!(session.tool_output_viewer_state().is_some());
    }

    #[test]
    fn raw_ctrl_t_opens_tool_output_viewer() {
        let mut session = Session::new(InlineTheme::default(), None, 24);

        let result = session.process_key(KeyEvent::new(KeyCode::Char('\u{14}'), KeyModifiers::empty()));

        assert!(result.is_none());
        assert!(session.tool_output_viewer_state().is_some());
    }

    #[test]
    fn unbound_ctrl_t_keeps_readline_transpose_outside_fullscreen() {
        let mut bindings = HashMap::new();
        bindings.insert("open_transcript_review".to_string(), Vec::new());
        let mut session = Session::new_with_logs_and_bindings(
            InlineTheme::default(),
            None,
            24,
            true,
            None,
            Vec::new(),
            "Agent TUI".to_string(),
            BindingStore::new(bindings),
        );
        session.core.input_manager.set_content("abc".to_string());
        session.core.input_manager.set_cursor(1);

        let result = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));

        assert!(result.is_none());
        assert_eq!(session.core.input_manager.content(), "bac");
        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn unbound_raw_ctrl_t_keeps_readline_transpose_outside_fullscreen() {
        let mut bindings = HashMap::new();
        bindings.insert("open_transcript_review".to_string(), Vec::new());
        let mut session = Session::new_with_logs_and_bindings(
            InlineTheme::default(),
            None,
            24,
            true,
            None,
            Vec::new(),
            "Agent TUI".to_string(),
            BindingStore::new(bindings),
        );
        session.core.input_manager.set_content("abc".to_string());
        session.core.input_manager.set_cursor(1);

        let result = session.process_key(KeyEvent::new(KeyCode::Char('\u{14}'), KeyModifiers::empty()));

        assert!(result.is_none());
        assert_eq!(session.core.input_manager.content(), "bac");
        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn unbound_transcript_review_does_not_restore_ctrl_t_viewer_alias() {
        let mut bindings = HashMap::new();
        bindings.insert("open_transcript_review".to_string(), Vec::new());
        let mut session = Session::new_with_logs_and_bindings(
            InlineTheme::default(),
            None,
            24,
            true,
            None,
            Vec::new(),
            "Agent TUI".to_string(),
            BindingStore::new(bindings),
        );
        session.core.set_fullscreen_active(true);
        add_compact_activity(&mut session, 40, "printf unbound");

        assert!(
            session
                .process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
                .is_none()
        );
        assert!(session.tool_output_viewer_state().is_none());

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render compact activity");
        assert!(session.compact_activity_hit_regions.is_empty());
    }

    #[test]
    fn transcript_review_binding_can_open_outside_fullscreen() {
        let mut bindings = HashMap::new();
        bindings.insert("open_transcript_review".to_string(), vec!["ctrl+x".to_string()]);
        let mut session = Session::new_with_logs_and_bindings(
            InlineTheme::default(),
            None,
            24,
            true,
            None,
            Vec::new(),
            "Agent TUI".to_string(),
            BindingStore::new(bindings),
        );

        assert!(!session.core.fullscreen.active);
        assert!(
            session
                .process_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))
                .is_none()
        );
        assert!(session.tool_output_viewer_state().is_some());
    }

    #[test]
    fn configured_core_action_dispatches_through_app_session() {
        let mut bindings = HashMap::new();
        bindings.insert("open_model_picker".to_string(), vec!["ctrl+x".to_string()]);
        let mut session = Session::new_with_logs_and_bindings(
            InlineTheme::default(),
            None,
            24,
            true,
            None,
            Vec::new(),
            "Agent TUI".to_string(),
            BindingStore::new(bindings),
        );

        assert!(matches!(
            session.process_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(InlineEvent::Submit(ref command)) if command == "/model"
        ));
    }

    #[test]
    fn raw_control_exit_is_normalized_before_app_dispatch() {
        let mut session = build_session();

        assert!(matches!(
            session.process_key(KeyEvent::new(KeyCode::Char('\u{4}'), KeyModifiers::NONE)),
            Some(InlineEvent::Exit)
        ));
    }

    #[test]
    fn transcript_review_render_mode_is_rebindable_inside_viewer() {
        let mut bindings = HashMap::new();
        bindings.insert("toggle_transcript_render_mode".to_string(), vec!["alt+x".to_string()]);
        let mut session = Session::new_with_logs_and_bindings(
            InlineTheme::default(),
            None,
            24,
            true,
            None,
            Vec::new(),
            "Agent TUI".to_string(),
            BindingStore::new(bindings),
        );
        session.core.set_fullscreen_active(true);

        assert!(
            session
                .process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
                .is_none()
        );
        let initial_status = session.tool_output_viewer_state().expect("viewer open").status_label();
        assert!(initial_status.contains("rich"));

        assert!(
            session
                .process_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))
                .is_none()
        );
        let raw_status = session.tool_output_viewer_state().expect("viewer open").status_label();
        assert!(raw_status.contains("raw"));
    }

    #[test]
    fn transcript_render_binding_does_not_consume_normal_input() {
        let mut session = build_session();

        assert!(
            session
                .process_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
                .is_none()
        );
        assert_eq!(session.core.input_manager.content(), "r");
        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn alt_o_compatibility_alias_opens_transcript_review() {
        let mut session = build_session();
        assert!(
            session
                .process_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT))
                .is_none()
        );
        assert!(session.tool_output_viewer_state().is_some());
    }

    #[test]
    fn compact_review_hint_click_opens_focused_transcript_review() {
        let mut session = build_session();
        add_compact_activity(&mut session, 41, "printf hello");
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render compact activity");

        let region = session
            .compact_activity_hit_regions
            .first()
            .copied()
            .expect("visible compact review hint should have a hit region");
        assert_eq!(region.review_anchor, 41);

        let (events, _received) = tokio::sync::mpsc::unbounded_channel();
        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: region.area.x,
                row: region.area.y,
                modifiers: KeyModifiers::NONE,
            }),
            &events,
            None,
        );

        assert!(session.tool_output_viewer_state().is_some());
    }

    #[test]
    fn compact_review_hint_hit_regions_survive_narrow_reflow() {
        let mut session = build_session();
        session.core.apply_transcript_width(12);
        add_compact_activity(&mut session, 45, "printf narrow");
        let backend = ratatui::backend::TestBackend::new(12, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render compact activity");

        assert!(!session.compact_activity_hit_regions.is_empty());
        assert!(
            session
                .compact_activity_hit_regions
                .iter()
                .all(|region| region.area.width > 0 && region.area.height == 1)
        );
    }

    #[test]
    fn compact_review_body_click_does_not_open_viewer() {
        let mut session = build_session();
        add_compact_activity(&mut session, 42, "printf body");
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render compact activity");
        let region = session
            .compact_activity_hit_regions
            .first()
            .copied()
            .expect("visible compact review hint should have a hit region");
        let body_column = region.area.x.saturating_sub(1);

        let (events, _received) = tokio::sync::mpsc::unbounded_channel();
        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: body_column,
                row: region.area.y,
                modifiers: KeyModifiers::NONE,
            }),
            &events,
            None,
        );

        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn expanded_pty_capture_anchors_to_live_header() {
        let mut session = build_session();
        session.handle_command(InlineCommand::AppendLine {
            kind: InlineMessageKind::Pty,
            segments: vec![text_segment("• Ran cargo check")],
        });
        session.handle_command(InlineCommand::RecordToolOutput {
            id: 47,
            lines: vec!["• Ran cargo check".to_string(), "  └ captured output".to_string()],
        });

        assert_eq!(session.tool_output_blocks[0].anchor_line, Some(0));
    }

    #[test]
    fn collapsing_pty_group_reanchors_only_group_members() {
        let mut session = build_session();
        add_compact_activity(&mut session, 44, "printf first");
        session.handle_command(InlineCommand::RecordToolOutput {
            id: 99,
            lines: vec!["• Ran failed command".to_string(), "    failed".to_string()],
        });
        session.handle_command(InlineCommand::AppendLine {
            kind: InlineMessageKind::Pty,
            segments: vec![text_segment("• Ran printf second")],
        });
        session.handle_command(InlineCommand::CollapsePtyBlock(CompactActivityMetadata {
            group_id: 44,
            command_count: 2,
            command: None,
            hidden_line_count: 2,
            suffix: None,
            review_anchor: Some(44),
            review_anchors: vec![44],
        }));

        assert_eq!(session.tool_output_blocks[0].anchor_line, Some(0));
        assert_eq!(session.tool_output_blocks[1].anchor_line, None);
        assert_eq!(session.compact_activity_entries.len(), 1);
        assert_eq!(session.compact_activity_entries[0].metadata.command_count, 2);
    }

    #[test]
    fn transcript_review_title_mode_click_toggles_rendering() {
        let mut session = build_session();
        add_compact_activity(&mut session, 43, "printf mode");
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render review");

        let mode_column = (0..80)
            .find(|column| {
                session
                    .tool_output_viewer_state()
                    .is_some_and(|viewer| viewer.mode_control_contains(*column, 0))
            })
            .expect("rendered review title should expose a mode hit region");
        let (events, _received) = tokio::sync::mpsc::unbounded_channel();
        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: mode_column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &events,
            None,
        );

        assert!(
            session
                .tool_output_viewer_state()
                .is_some_and(|viewer| viewer.render_mode() == tool_output_viewer::TranscriptRenderMode::Raw)
        );
    }

    #[test]
    fn transcript_review_header_close_button_closes_on_mouse_click_and_shows_guide() {
        let mut session = build_session();
        add_compact_activity(&mut session, 46, "printf close");
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render review");

        let close_column = (0..80)
            .find(|column| {
                session
                    .tool_output_viewer_state()
                    .is_some_and(|viewer| viewer.close_control_contains(*column, 0))
            })
            .expect("rendered review title should expose a close hit region");
        let rendered = rendered_buffer_text(&terminal);
        assert!(rendered.contains("[close]"));
        assert!(rendered.contains("Ctrl+T open/close"));
        assert!(rendered.contains("R rich/raw"));
        assert!(rendered.contains("Esc close"));

        let (events, _received) = tokio::sync::mpsc::unbounded_channel();
        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: close_column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &events,
            None,
        );

        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn alt_t_still_toggles_tool_display_mode() {
        let mut session = build_session();

        assert!(matches!(
            session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT)),
            Some(InlineEvent::ToggleToolDisplayMode)
        ));
    }

    #[test]
    fn alt_g_toggles_task_panel() {
        let mut session = build_session();
        assert!(!session.show_task_panel);

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT));
        assert!(session.show_task_panel, "Alt+G should reveal the task panel");

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT));
        assert!(!session.show_task_panel, "Second Alt+G should hide the task panel");
    }

    #[test]
    fn ctrl_home_and_end_jump_transcript_in_fullscreen() {
        let mut session = build_session();
        for index in 0..40 {
            session
                .core
                .push_line(InlineMessageKind::Agent, vec![text_segment(format!("line {index}"))]);
        }

        session.core.scroll_page_up();
        assert!(session.core.scroll_offset() > 0);

        let _ = session.process_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(session.core.scroll_offset(), 0);

        let _ = session.process_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(session.core.scroll_offset(), session.core.current_max_scroll_offset());
    }

    #[test]
    fn tool_output_viewer_search_accept_and_cancel_work() {
        let mut session = build_session();
        session.tool_output_blocks.push(ToolOutputBlock {
            lines: vec![
                "• Ran alpha".to_string(),
                "  └ beta alpha".to_string(),
                "  └ gamma alpha".to_string(),
            ],
            ..Default::default()
        });
        session.tool_output_revision = 1;

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for ch in ['a', 'l', 'p', 'h', 'a'] {
            let _ = session.process_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let _ = session.process_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        let status = session.tool_output_viewer_state().expect("viewer open").status_label();
        assert!(status.contains("search 'alpha'"));
        assert!(status.contains("(2/3)"));

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        let _ = session.process_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let status = session.tool_output_viewer_state().expect("viewer open").status_label();
        assert!(status.contains("search 'alpha'"));
    }

    #[test]
    fn tool_output_viewer_exports_complete_tool_output() {
        let mut session = build_session();
        session.tool_output_blocks.push(ToolOutputBlock {
            lines: vec![
                "• Ran printf complete".to_string(),
                "  └ first complete line".to_string(),
                "    second complete line".to_string(),
            ],
            ..Default::default()
        });
        session.tool_output_revision = 1;

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));

        match session.process_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE)) {
            Some(InlineEvent::OpenToolOutputInEditor(text)) => {
                assert!(text.contains("first complete line"));
                assert!(text.contains("second complete line"));
            }
            other => panic!("unexpected viewer editor event: {other:?}"),
        }

        match session.process_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE)) {
            Some(InlineEvent::OpenToolOutputScrollback(text)) => {
                assert!(text.contains("first complete line"));
                assert!(text.contains("second complete line"));
            }
            other => panic!("unexpected viewer scrollback event: {other:?}"),
        }
    }

    #[test]
    fn tool_output_viewer_scrolls_copies_selection_and_closes() {
        let mut session = build_session();
        session.tool_output_blocks.push(ToolOutputBlock {
            lines: (0..20).map(|index| format!("output line {index}")).collect(),
            ..Default::default()
        });
        session.tool_output_revision = 1;

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let initial_status = session.tool_output_viewer_state().expect("viewer open").status_label();
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        let top_status = session.tool_output_viewer_state().expect("viewer open").status_label();
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let next_status = session.tool_output_viewer_state().expect("viewer open").status_label();

        assert!(initial_status.contains("line 13/20"));
        assert!(top_status.contains("line 1/20"));
        assert!(next_status.contains("line 2/20"));

        session.core.mouse_selection.set_selection((1, 1), (8, 1));
        let _ = session.process_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(session.core.mouse_selection.has_copy_request());

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(session.tool_output_viewer_state().is_none());
    }

    #[test]
    fn tool_output_viewer_mouse_scroll_does_not_move_transcript() {
        let mut session = build_session();
        session.tool_output_blocks.push(ToolOutputBlock {
            lines: (0..20).map(|index| format!("output line {index}")).collect(),
            ..Default::default()
        });
        session.tool_output_revision = 1;
        for index in 0..20 {
            session
                .core
                .push_line(InlineMessageKind::Agent, vec![text_segment(format!("transcript line {index}"))]);
        }

        let _ = session.process_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let transcript_offset = session.core.scroll_offset();
        let viewer_status = session.tool_output_viewer_state().expect("viewer open").status_label();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &tx,
            None,
        );

        assert_eq!(session.core.scroll_offset(), transcript_offset);
        assert_ne!(session.tool_output_viewer_state().expect("viewer open").status_label(), viewer_status);
    }

    #[test]
    fn mouse_events_are_ignored_when_fullscreen_mouse_capture_is_disabled() {
        let mut session = build_session();
        session.core.fullscreen.interaction.mouse_capture = false;
        for index in 0..20 {
            session
                .core
                .push_line(InlineMessageKind::Agent, vec![text_segment(format!("line {index}"))]);
        }
        session.core.scroll_page_up();
        let initial_offset = session.core.scroll_offset();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &tx,
            None,
        );

        assert_eq!(session.core.scroll_offset(), initial_offset);
    }

    #[test]
    fn transcript_wheel_scrolls_through_floating_plan_overlay() {
        let mut session = build_session();
        for index in 0..40 {
            session
                .core
                .push_line(InlineMessageKind::Agent, vec![text_segment(format!("plan line {index}"))]);
        }
        session.show_transient(TransientRequest::List(crate::tui::core_tui::app::types::ListOverlayRequest {
            title: "Ready to code?".to_string(),
            lines: vec!["A plan is ready to execute.".to_string()],
            footer_hint: None,
            items: vec![crate::tui::core_tui::types::InlineListItem {
                title: "Yes".to_string(),
                subtitle: None,
                badge: None,
                indent: 0,
                selection: None,
                search_value: None,
            }],
            selected: None,
            search: None,
            hotkeys: Vec::new(),
        }));

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| session.render(frame)).expect("render overlay");
        let (events, _received) = tokio::sync::mpsc::unbounded_channel();

        session.handle_event(
            CrosstermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &events,
            None,
        );

        assert!(session.core.scroll_offset() > 0, "transcript should remain scrollable behind the overlay");
    }

    fn build_session_with_secure_prompt() -> Session {
        let mut session = build_session();
        session.show_transient(TransientRequest::Modal(ModalOverlayRequest {
            title: "Secure API key setup".to_string(),
            lines: vec!["Paste the key — it will be auto-detected and saved securely.".to_string()],
            secure_prompt: Some(SecurePromptConfig {
                label: "API key".to_string(),
                placeholder: None,
                mask_input: true,
            }),
        }));
        assert!(session.has_active_overlay(), "secure prompt modal should be open");
        session
    }

    #[test]
    fn secure_prompt_modal_typing_populates_input_buffer() {
        let mut session = build_session_with_secure_prompt();

        // Typed characters must reach the input buffer, not be silently consumed.
        for ch in ['s', 'k', '-', 't', 'e', 's', 't'] {
            let event = session.process_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            assert!(event.is_none(), "character '{ch}' should not emit an event");
        }

        assert_eq!(session.core.input_manager.content(), "sk-test");
        assert!(session.has_active_overlay(), "modal should remain open while typing");
    }

    #[test]
    fn secure_prompt_modal_enter_submits_and_closes() {
        let mut session = build_session_with_secure_prompt();

        for ch in ['s', 'k', '-', 't', 'e', 's', 't'] {
            let _ = session.process_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        let event = session.process_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match event {
            Some(InlineEvent::Submit(input)) => {
                assert_eq!(&*input, "sk-test", "Enter should submit the typed API key");
            }
            other => panic!("Enter should emit Submit, got {other:?}"),
        }
        assert!(!session.has_active_overlay(), "modal should be closed after Enter");
        assert!(session.core.input_manager.content().is_empty(), "input buffer should be cleared after submit");
    }

    #[test]
    fn secure_prompt_modal_esc_cancels_and_closes() {
        let mut session = build_session_with_secure_prompt();

        for ch in ['s', 'k', '-', 't', 'e', 's', 't'] {
            let _ = session.process_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        let event = session.process_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(event.is_none(), "Esc should not emit a submit event, got {event:?}");
        assert!(!session.has_active_overlay(), "modal should be closed after Esc");
        assert!(session.core.input_manager.content().is_empty(), "input buffer should be cleared after Esc");
    }

    #[test]
    fn secure_prompt_modal_paste_auto_submits_and_closes() {
        let mut session = build_session_with_secure_prompt();

        let event = handle_paste(&mut session, "sk-pasted-key\n");

        match event {
            Some(InlineEvent::Submit(input)) => {
                assert_eq!(&*input, "sk-pasted-key", "paste should auto-submit trimmed key");
            }
            other => panic!("paste should emit Submit, got {other:?}"),
        }
        assert!(!session.has_active_overlay(), "modal should be closed after paste");
    }

    #[test]
    fn secure_prompt_modal_enter_with_empty_input_does_not_close() {
        let mut session = build_session_with_secure_prompt();

        // Pressing Enter without typing anything should not close the modal.
        let event = session.process_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(event.is_none(), "Enter with empty input should not emit an event");
        assert!(session.has_active_overlay(), "modal should remain open when input is empty");
    }

    #[test]
    fn secure_prompt_modal_backspace_and_ctrl_u_edit_input() {
        let mut session = build_session_with_secure_prompt();

        for ch in ['s', 'k', '-', 't', 'e', 's', 't'] {
            let _ = session.process_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(session.core.input_manager.content(), "sk-test");

        // Backspace deletes the last character.
        let event = session.process_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(event.is_none(), "Backspace should not emit an event");
        assert_eq!(session.core.input_manager.content(), "sk-tes");
        assert!(session.has_active_overlay(), "modal should remain open while editing");

        // Ctrl+U clears from cursor to start of line.
        let event = session.process_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(event.is_none(), "Ctrl+U should not emit an event");
        assert!(session.core.input_manager.content().is_empty(), "Ctrl+U should clear the buffer");
        assert!(session.has_active_overlay(), "modal should remain open after Ctrl+U");
    }

    #[test]
    fn secure_prompt_modal_consumes_composer_shortcuts() {
        let mut session = build_session_with_secure_prompt();

        // Ctrl+M would normally submit "/model" from the composer; inside a secure
        // prompt it must be consumed so the modal stays open and no event leaks.
        let event = session.process_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL));
        assert!(event.is_none(), "Ctrl+M must not leak through the secure prompt, got {event:?}");
        assert!(session.has_active_overlay(), "modal should remain open when Ctrl+M is pressed");

        // Tab is consumed (not inserted, no agent cycling) while the secure prompt is open.
        let event = session.process_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(event.is_none(), "Tab must not leak through the secure prompt, got {event:?}");
        assert!(session.core.input_manager.content().is_empty(), "Tab must not insert a character");
        assert!(session.has_active_overlay(), "modal should remain open when Tab is pressed");
    }
}
