mod docs;
mod items;
mod mutations;
mod path;
mod render;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::config_section_headings::{heading_for_path, humanize_identifier};
use anyhow::{Context, Result, anyhow, bail};
use diapason_core::config::loader::DiapasonConfig;
use diapason_core::config::{ConfigResetRequest, ConfigService, ConfigWriteTarget};
use diapason_core::utils::ansi::AnsiRenderer;
use diapason_ui::tui::app::{InlineListItem, InlineListSearchConfig, InlineListSelection};
use path::get_node;
use render::summarize_value;
use toml::Value as TomlValue;

#[cfg(test)]
use docs::FIELD_DOCS;
use items::build_settings_items;
pub(crate) use mutations::reload_state_from_disk;
use mutations::{
    ScalarOperation, add_array_item, apply_scalar_operation, mutate_draft_and_persist, no_config_source_label,
    pop_array_item,
};
#[cfg(test)]
use mutations::{mutate_draft, render_commented_config};
pub(crate) use path::parent_view_path;
#[cfg(test)]
use path::{PathToken, parse_path_tokens};

const SETTINGS_TITLE: &str = "Diapason Settings";
const SETTINGS_HINT: &str = "Enter open/apply • ←/→ adjust • Esc back";
const SETTINGS_SEARCH_PLACEHOLDER: &str = "section, setting, or value";
pub(crate) const ACTION_RELOAD: &str = "settings:reload";
pub(crate) const ACTION_OPEN_ROOT: &str = "settings:open_root";
pub(crate) const ACTION_RESET: &str = "settings:reset";
pub(crate) const ACTION_RESET_CONFIRM: &str = "settings:reset_confirm";
pub(crate) const ACTION_RESET_CANCEL: &str = "settings:reset_cancel";
const ACTION_PREFIX_OPEN: &str = "settings:open:";
const ACTION_PREFIX_ARRAY_ADD: &str = "settings:array_add:";
const ACTION_PREFIX_ARRAY_POP: &str = "settings:array_pop:";
const ACTION_PREFIX_SET: &str = "settings:set:";
const OPTIONAL_DOC_FIELDS: &[&str] = &["provider.anthropic.thinking_display", "provider.openai.service_tier"];
pub(crate) const SETTINGS_MODEL_CONFIG_PATH: &str = "model_config";
pub(crate) const SETTINGS_MODEL_CONFIG_MAIN_PATH: &str = "model_config.main";
pub(crate) const ACTION_PICK_MAIN_MODEL: &str = "settings:pick_main_model";
pub(crate) const ACTION_CONFIGURE_EDITOR: &str = "settings:configure_editor";
const RESET_CONFIRMATION_VIEW: &str = "__settings_reset_confirmation";

#[derive(Clone)]
pub(crate) struct SettingsPaletteState {
    pub(crate) workspace: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) source_label: String,
    pub(crate) draft: DiapasonConfig,
    pub(crate) view_path: Option<String>,
    /// Last submitted selection in the current settings view.
    pub(crate) last_selection: Option<InlineListSelection>,
    /// Selection memory keyed by view path; the empty key is the root view.
    pub(crate) selection_by_view: BTreeMap<String, InlineListSelection>,
}

impl SettingsPaletteState {
    fn view_key(view_path: Option<&str>) -> &str {
        view_path.unwrap_or("")
    }

    pub(crate) fn remember_selection(&mut self, view_path: Option<&str>, selection: InlineListSelection) {
        self.last_selection = Some(selection.clone());
        self.selection_by_view.insert(Self::view_key(view_path).to_string(), selection);
    }

    pub(crate) fn selection_for_view(&self, view_path: Option<&str>) -> Option<InlineListSelection> {
        self.selection_by_view.get(Self::view_key(view_path)).cloned()
    }
}

#[derive(Debug, Default)]
pub(crate) struct SettingsApplyOutcome {
    pub(crate) message: Option<String>,
    pub(crate) saved: bool,
}

pub(crate) fn create_settings_palette_state(
    workspace: &Path,
    vt_snapshot: &Option<DiapasonConfig>,
) -> Result<SettingsPaletteState> {
    let manager = crate::main_helpers::load_workspace_config(workspace)?;
    let has_config_file = manager.config_path().is_some();
    let source_path = manager
        .config_path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.join("config.toml"));

    let draft = if has_config_file {
        manager.config().clone()
    } else {
        vt_snapshot.clone().unwrap_or_else(|| manager.config().clone())
    };

    let source_label = if has_config_file {
        format!("Configuration source: {}", source_path.display())
    } else {
        no_config_source_label(workspace)
    };

    Ok(SettingsPaletteState {
        workspace: workspace.to_path_buf(),
        source_path,
        source_label,
        draft,
        view_path: None,
        last_selection: None,
        selection_by_view: BTreeMap::new(),
    })
}

pub(crate) fn show_settings_palette(
    renderer: &mut AnsiRenderer,
    state: &SettingsPaletteState,
    selected: Option<InlineListSelection>,
) -> Result<bool> {
    let draft_value = TomlValue::try_from(state.draft.clone()).context("Failed to serialize draft configuration")?;

    let mut lines = Vec::new();
    lines.push(state.source_label.clone());
    lines.push(format!("Target file: {}", state.source_path.display()));
    if state.view_path.as_deref() == Some(RESET_CONFIRMATION_VIEW) {
        lines.push("Reset configuration? This clears every setting in the target layer.".to_string());
        lines.push("Credentials and lower-precedence configuration layers are preserved.".to_string());
    } else if let Some(view_path) = state.view_path.as_deref() {
        let heading = heading_for_path(view_path);
        lines.push(format!("Settings / {} ({})", heading.title, display_settings_view_path(view_path)));
        if !heading.summary.is_empty() {
            lines.push(heading.summary.into_owned());
        }
        if view_path == "permissions" {
            lines.push(format_permission_summary(&state.draft));
        }
    } else {
        lines.push("Settings / Sections".to_string());
        lines.push("Choose a category to edit. Each entry shows its effective value and available action.".to_string());
    }
    lines.push(SETTINGS_HINT.to_string());

    let items = build_settings_items(state, &draft_value)?;
    if items.is_empty() {
        return Ok(false);
    }

    let selected = preferred_settings_selection(state, &items, selected);
    renderer.show_list_modal(
        SETTINGS_TITLE,
        lines,
        items,
        selected,
        Some(InlineListSearchConfig {
            label: String::new(),
            placeholder: Some(SETTINGS_SEARCH_PLACEHOLDER.to_string()),
        }),
    );

    Ok(true)
}

fn preferred_settings_selection(
    state: &SettingsPaletteState,
    items: &[InlineListItem],
    selected: Option<InlineListSelection>,
) -> Option<InlineListSelection> {
    let selected = if state.view_path.as_deref() == Some(RESET_CONFIRMATION_VIEW) {
        Some(InlineListSelection::ConfigAction(ACTION_RESET_CONFIRM.to_string()))
    } else {
        selected
    };
    selected
        .into_iter()
        .chain(state.selection_for_view(state.view_path.as_deref()))
        .find(|candidate| items.iter().any(|item| item.selection.as_ref() == Some(candidate)))
        .or_else(|| items.iter().find_map(|item| item.selection.clone()))
}

fn format_permission_summary(config: &DiapasonConfig) -> String {
    format!(
        "Rules: deny: {} | ask: {} | allow: {}",
        config.permissions.deny.len(),
        config.permissions.ask.len(),
        config.permissions.allow.len()
    )
}

pub(crate) fn apply_settings_action(state: &mut SettingsPaletteState, action: &str) -> Result<SettingsApplyOutcome> {
    let mut outcome = SettingsApplyOutcome::default();

    if matches!(action, ACTION_PICK_MAIN_MODEL | ACTION_CONFIGURE_EDITOR) {
        return Ok(outcome);
    }

    match action {
        ACTION_RELOAD => {
            reload_state_from_disk(state)?;
            outcome.message = Some("Reloaded settings from disk.".to_string());
            return Ok(outcome);
        }
        ACTION_RESET => {
            state.view_path = Some(RESET_CONFIRMATION_VIEW.to_string());
            outcome.message = Some(format!("Confirm reset to clear all settings in {}.", state.source_path.display()));
            return Ok(outcome);
        }
        ACTION_RESET_CANCEL => {
            state.view_path = None;
            outcome.message = Some("Configuration reset cancelled.".to_string());
            return Ok(outcome);
        }
        ACTION_RESET_CONFIRM => {
            let response = ConfigService::reset(ConfigResetRequest {
                workspace: state.workspace.clone(),
                target: ConfigWriteTarget::Workspace,
                expected_layer_version: None,
                path: Some(state.source_path.clone()),
            })?;
            state.draft = serde_json::from_value(response.effective_config)
                .context("Reset configuration could not be converted to the effective settings")?;
            state.view_path = None;
            state.source_label = format!("Configuration source: {}", response.path.display());
            outcome.saved = true;
            outcome.message = Some(format!("Reset configuration at {}.", response.path.display()));
            return Ok(outcome);
        }
        ACTION_OPEN_ROOT => {
            state.view_path = None;
            return Ok(outcome);
        }
        _ => {}
    }

    if let Some(path) = action.strip_prefix(ACTION_PREFIX_OPEN) {
        if path.trim().is_empty() {
            state.view_path = None;
        } else {
            state.view_path = Some(path.to_string());
        }
        return Ok(outcome);
    }

    if let Some(path) = action.strip_prefix(ACTION_PREFIX_ARRAY_ADD) {
        mutate_draft_and_persist(state, path, |draft| add_array_item(draft, path))?;
        outcome.saved = true;
        outcome.message = Some(describe_array_change(path, true));
        return Ok(outcome);
    }

    if let Some(path) = action.strip_prefix(ACTION_PREFIX_ARRAY_POP) {
        mutate_draft_and_persist(state, path, |draft| pop_array_item(draft, path))?;
        outcome.saved = true;
        outcome.message = Some(describe_array_change(path, false));
        return Ok(outcome);
    }

    if let Some(rest) = action.strip_prefix(ACTION_PREFIX_SET) {
        let (path, op) = rest
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("Invalid settings action: {action}"))?;

        let operation = match op {
            "toggle" => ScalarOperation::Toggle,
            "inc" => ScalarOperation::Increment,
            "dec" => ScalarOperation::Decrement,
            "cycle" => ScalarOperation::CycleNext,
            "cycle_prev" => ScalarOperation::CyclePrev,
            _ => bail!("Unsupported settings operation: {op}"),
        };

        mutate_draft_and_persist(state, path, |draft| apply_scalar_operation(draft, path, operation))?;
        outcome.saved = true;
        outcome.message = Some(describe_scalar_change(state, path, operation));
        return Ok(outcome);
    }

    bail!("Unknown settings action: {action}")
}

pub(crate) fn resolve_settings_view_path(path: &str) -> String {
    match path.trim() {
        "model" => SETTINGS_MODEL_CONFIG_PATH.to_string(),
        "model.main" => SETTINGS_MODEL_CONFIG_MAIN_PATH.to_string(),
        "codex" | "codex_app_server" | "codex.app_server" | "app_server" => "agent.codex_app_server".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn display_settings_view_path(path: &str) -> Cow<'_, str> {
    match path {
        SETTINGS_MODEL_CONFIG_PATH => Cow::Borrowed("model"),
        SETTINGS_MODEL_CONFIG_MAIN_PATH => Cow::Borrowed("model.main"),
        other => Cow::Borrowed(other),
    }
}

/// Human-readable label for a settings path, used in change feedback messages.
///
/// For generic boolean leaf names (`enabled`, `show`, ...) the parent section is
/// used instead so feedback reads naturally (e.g. "Disabled IDE Context" rather
/// than "Disabled Enabled").
fn change_title(path: &str) -> String {
    let last_segment = path
        .rsplit('.')
        .next()
        .unwrap_or(path)
        .trim_end_matches(|c: char| c.is_numeric() || c == ']' || c == '[');
    let leaf = humanize_identifier(last_segment);

    const GENERIC_BOOL_LEAVES: &[&str] = &["Enabled", "Disabled", "Show", "Hide", "On", "Off"];
    if GENERIC_BOOL_LEAVES.contains(&leaf.as_str()) {
        if let Some(parent) = path.rsplit_once('.') {
            let parent_leaf = parent
                .0
                .rsplit('.')
                .next()
                .unwrap_or(parent.0)
                .trim_end_matches(|c: char| c.is_numeric() || c == ']' || c == '[');
            return humanize_identifier(parent_leaf);
        }
    }

    leaf
}

/// Builds a status-line message describing a scalar config change.
fn describe_scalar_change(state: &SettingsPaletteState, path: &str, operation: ScalarOperation) -> String {
    let title = change_title(path);
    let draft_value = TomlValue::try_from(state.draft.clone()).ok();
    let value = draft_value.as_ref().and_then(|value| get_node(value, path));

    match (operation, value) {
        (ScalarOperation::Toggle, Some(TomlValue::Boolean(enabled))) => {
            format!("{} {}", if *enabled { "Enabled" } else { "Disabled" }, title)
        }
        (_, Some(value)) => format!("{} → {}", title, summarize_value(value)),
        _ => format!("Updated {}", title),
    }
}

/// Builds a status-line message describing an array add/remove change.
fn describe_array_change(path: &str, added: bool) -> String {
    let title = change_title(path);
    if added {
        format!("Added item to {}", title)
    } else {
        format!("Removed last item from {}", title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::runloop::unified::config_section_headings::normalize_config_path;
    use diapason_commons::canonicalize;
    use diapason_commons::reference::StaticWorkspacePaths;
    use diapason_config::defaults::WorkspacePathsDefaults;
    use diapason_config::defaults::provider::with_config_defaults_provider_for_test;
    use diapason_core::config::ConfigManager;
    use serial_test::serial;
    use std::sync::Arc;

    #[test]
    fn parse_path_handles_arrays() {
        let tokens = parse_path_tokens("commands.allow_list[1]").expect("tokens");
        assert_eq!(tokens.len(), 3);
        matches!(tokens[0], PathToken::Key(_));
        matches!(tokens[1], PathToken::Key(_));
        matches!(tokens[2], PathToken::Index(1));
    }

    #[test]
    fn normalize_field_path_replaces_indexes() {
        assert_eq!(normalize_config_path("commands.allow_list[12]"), "commands.allow_list[]");
    }

    #[test]
    fn parent_view_path_handles_nested_segments() {
        assert_eq!(parent_view_path("agent"), None);
        assert_eq!(parent_view_path("agent.vibe_coding"), Some("agent".to_string()));
        assert_eq!(
            parent_view_path("hooks.lifecycle.pre_tool_use[0].hooks[2]"),
            Some("hooks.lifecycle.pre_tool_use[0].hooks".to_string())
        );
    }

    #[test]
    fn settings_selection_memory_restores_parent_and_falls_back_when_removed() {
        let mut state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let root_selection = InlineListSelection::ConfigAction("settings:open:agent".to_string());
        let nested_selection = InlineListSelection::ConfigAction("settings:set:agent.provider:cycle".to_string());

        state.remember_selection(None, root_selection.clone());
        state.view_path = Some("agent".to_string());
        state.remember_selection(Some("agent"), nested_selection.clone());

        assert_eq!(state.selection_for_view(Some("agent")), Some(nested_selection));
        assert_eq!(state.selection_for_view(None), Some(root_selection));
        assert_eq!(
            state.last_selection,
            Some(InlineListSelection::ConfigAction("settings:set:agent.provider:cycle".to_string(),))
        );

        let draft = TomlValue::try_from(state.draft.clone()).expect("default config should serialize");
        let items = build_settings_items(&state, &draft).expect("settings items");
        let removed_selection = InlineListSelection::ConfigAction("settings:set:agent.removed:cycle".to_string());
        state.selection_by_view.remove("agent");
        let fallback = preferred_settings_selection(&state, &items, Some(removed_selection));
        assert_eq!(fallback, items.iter().find_map(|item| item.selection.clone()));
    }

    #[test]
    fn reset_confirmation_lists_only_confirm_and_cancel_actions() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some(RESET_CONFIRMATION_VIEW.to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(state.draft.clone()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("reset confirmation items");
        let actions = items.iter().filter_map(|item| item.selection.as_ref()).collect::<Vec<_>>();
        assert_eq!(actions.len(), 2);
        assert!(actions.contains(&&InlineListSelection::ConfigAction(ACTION_RESET_CONFIRM.to_string())));
        assert!(actions.contains(&&InlineListSelection::ConfigAction(ACTION_RESET_CANCEL.to_string())));
    }

    #[test]
    fn parse_field_docs_has_known_entry() {
        assert!(FIELD_DOCS.lookup("agent.provider").is_some());
    }

    #[test]
    fn tool_display_mode_is_exposed_as_a_cycle_setting() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("ui".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(state.draft.clone()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let item = items
            .iter()
            .find(|item| item.title == "Tool Display Mode")
            .expect("tool display mode entry");
        assert_eq!(
            item.selection,
            Some(InlineListSelection::ConfigAction("settings:set:ui.tool_display_mode:cycle".to_string()))
        );
        assert_eq!(item.badge.as_deref(), Some("Pick"));
        assert!(item.subtitle.as_deref().is_some_and(|subtitle| subtitle.contains("expanded")));
        assert!(item.subtitle.as_deref().is_some_and(|subtitle| subtitle.contains("compact")));
    }

    #[test]
    fn tool_display_mode_cycle_persists_to_disk() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("config.toml");
        let mut state = SettingsPaletteState {
            workspace: temp.path().to_path_buf(),
            source_path: source_path.clone(),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("ui".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };

        let outcome = apply_settings_action(&mut state, "settings:set:ui.tool_display_mode:cycle")
            .expect("cycle tool display mode");

        assert!(outcome.saved);
        // The default is compact; cycling advances to expanded.
        assert_eq!(state.draft.ui.tool_display_mode, diapason_core::config::ToolDisplayMode::Expanded);
        let persisted = std::fs::read_to_string(&source_path).expect("persisted config");
        assert!(persisted.contains("tool_display_mode = \"expanded\""));
    }

    #[test]
    #[serial]
    fn settings_palette_uses_explicit_session_override_as_source() {
        use diapason_config::loader::set_explicit_config_path;

        struct OverrideGuard;

        impl OverrideGuard {
            fn set(path: Option<PathBuf>) -> Self {
                set_explicit_config_path(path);
                Self
            }
        }

        impl Drop for OverrideGuard {
            fn drop(&mut self) {
                set_explicit_config_path(None);
            }
        }

        let temp = tempfile::tempdir().expect("temp dir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::write(workspace.join("config.toml"), "agent.provider = \"anthropic\"\n").expect("workspace config");

        let override_path = temp.path().join("custom-night.toml");
        std::fs::write(&override_path, "agent.provider = \"openai\"\n").expect("override config");

        let _guard = OverrideGuard::set(Some(override_path.clone()));
        let state = create_settings_palette_state(&workspace, &None);

        let state = state.expect("settings state with override");
        assert_eq!(
            canonicalize(&state.source_path).expect("canonical source path"),
            canonicalize(&override_path).expect("canonical override path"),
            "settings palette must treat the explicit override file as its source"
        );
        assert_eq!(state.draft.agent.provider, "openai");
    }

    #[test]
    fn root_settings_items_include_nested_keys_for_global_search() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft: TomlValue = toml::from_str(
            r#"
            [tools.editor]
            preferred_editor = "code --wait"
            [agent]
            quiet = true
            "#,
        )
        .expect("valid draft value");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let tools_entry = items
            .iter()
            .find(|item| item.title == "Tool Defaults")
            .expect("root should show section heading");
        let search_value = tools_entry.search_value.as_deref().expect("search value");
        assert!(search_value.contains("tools.editor.preferred_editor"));
        assert!(search_value.contains("code --wait"));
    }

    #[test]
    fn root_settings_hide_nested_items_until_section_is_opened() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft: TomlValue = toml::from_str(
            r#"
            [tools.editor]
            preferred_editor = "code --wait"
            "#,
        )
        .expect("valid draft value");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "Tool Defaults"));
        assert!(!items.iter().any(|item| item.title == "Preferred Editor"));
    }

    #[test]
    fn root_settings_do_not_show_missing_nested_optional_fields() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(!items.iter().any(|item| item.title == "Service Tier"));
    }

    #[test]
    fn nested_settings_titles_are_humanized() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("agent".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft: TomlValue = toml::from_str(
            r#"
            [agent]
            default_model = "gpt-5.6-sol"
            [agent.circuit_breaker]
            enabled = true
            "#,
        )
        .expect("valid draft value");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "Default Model"));
        assert!(items.iter().any(|item| item.title == "Circuit Breaker"));
    }

    #[test]
    fn agent_view_hides_deprecated_auto_permissions_field() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("agent".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(!items.iter().any(|item| item.title == "Autonomous Execution"));
    }

    #[test]
    fn provider_openai_view_includes_missing_service_tier_doc_field() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("provider.openai".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "Service Tier"));
    }

    #[test]
    fn render_commented_config_includes_section_heading() {
        let mut config = DiapasonConfig::default();
        config.agent.default_model = "gpt-5.6-sol".to_string();

        let rendered = render_commented_config(&config).expect("config should render");
        assert!(rendered.contains("# Agent Defaults"));
        assert!(rendered.contains("[agent]"));
    }

    #[test]
    fn missing_service_tier_cycle_creates_value() {
        let mut state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("provider.openai".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };

        mutate_draft(&mut state, |draft| {
            apply_scalar_operation(draft, "provider.openai.service_tier", ScalarOperation::CycleNext)
        })
        .expect("service tier should be inserted");

        assert_eq!(state.draft.provider.openai.service_tier, Some(diapason_config::OpenAIServiceTier::Flex));
    }

    #[test]
    fn service_tier_cycle_advances_from_flex_to_priority() {
        let mut state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("provider.openai".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        state.draft.provider.openai.service_tier = Some(diapason_config::OpenAIServiceTier::Flex);

        mutate_draft(&mut state, |draft| {
            apply_scalar_operation(draft, "provider.openai.service_tier", ScalarOperation::CycleNext)
        })
        .expect("service tier should advance");

        assert_eq!(state.draft.provider.openai.service_tier, Some(diapason_config::OpenAIServiceTier::Priority));
    }

    #[test]
    fn root_settings_include_ide_context_section() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "IDE Context"));
        assert!(items.iter().any(|item| item.title == "Custom Providers"));
    }

    #[test]
    fn resolve_settings_view_path_maps_model_aliases() {
        assert_eq!(resolve_settings_view_path("model"), SETTINGS_MODEL_CONFIG_PATH);
        assert_eq!(resolve_settings_view_path("model.main"), SETTINGS_MODEL_CONFIG_MAIN_PATH);
        assert_eq!(resolve_settings_view_path("codex"), "agent.codex_app_server");
        assert_eq!(resolve_settings_view_path("codex_app_server"), "agent.codex_app_server");
    }

    #[test]
    fn root_settings_include_model_config_quick_access() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let entry = items
            .iter()
            .find(|item| item.title == "Model Config")
            .expect("model config quick access");
        assert_eq!(
            entry.selection,
            Some(InlineListSelection::ConfigAction(format!("{ACTION_PREFIX_OPEN}{SETTINGS_MODEL_CONFIG_PATH}")))
        );
    }

    #[test]
    fn root_settings_include_external_editor_quick_access() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let entry = items
            .iter()
            .find(|item| item.title == "External Editor")
            .expect("external editor quick access");
        assert_eq!(entry.selection, Some(InlineListSelection::ConfigAction(ACTION_CONFIGURE_EDITOR.to_string())));
    }

    #[test]
    fn root_settings_include_codex_app_server_quick_access() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: None,
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let entry = items
            .iter()
            .find(|item| item.title == "Codex App Server")
            .expect("codex app server quick access");
        assert_eq!(
            entry.selection,
            Some(InlineListSelection::ConfigAction("settings:open:agent.codex_app_server".to_string()))
        );
    }

    #[test]
    fn codex_app_server_custom_command_entry_is_cycleable() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("agent.codex_app_server".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft: TomlValue = toml::from_str(
            r#"
            [agent.codex_app_server]
            command = "/usr/local/bin/codex"
            args = ["app-server"]
            startup_timeout_secs = 10
            experimental_features = false
            "#,
        )
        .expect("valid draft value");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let entry = items.iter().find(|item| item.title == "Command").expect("command entry");
        assert_eq!(
            entry.selection,
            Some(InlineListSelection::ConfigAction("settings:set:agent.codex_app_server.command:cycle".to_string()))
        );
    }

    #[test]
    fn model_config_root_shows_main_section() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some(SETTINGS_MODEL_CONFIG_PATH.to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "Main Model"));
        assert!(!items.iter().any(|item| item.title == "Lightweight Model"));
    }

    #[test]
    fn model_config_main_uses_picker_backed_default_model() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some(SETTINGS_MODEL_CONFIG_MAIN_PATH.to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "Provider"));
        let default_model = items
            .iter()
            .find(|item| item.title == "Default Model")
            .expect("default model entry");
        assert_eq!(
            default_model.selection,
            Some(InlineListSelection::ConfigAction(ACTION_PICK_MAIN_MODEL.to_string()))
        );
    }

    #[test]
    fn ide_context_view_includes_provider_section_navigation() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("ide_context.providers".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        assert!(items.iter().any(|item| item.title == "VS Code Family"));
        assert!(items.iter().any(|item| item.title == "Zed Family"));
        assert!(items.iter().any(|item| item.title == "Generic Bridge"));
    }

    #[test]
    fn tools_view_routes_external_editor_to_configure_action() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("tools".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let entry = items
            .iter()
            .find(|item| item.title == "External Editor")
            .expect("tools.external editor entry");
        assert_eq!(entry.selection, Some(InlineListSelection::ConfigAction(ACTION_CONFIGURE_EDITOR.to_string())));
    }

    #[test]
    fn agent_view_uses_picker_backed_default_model() {
        let state = SettingsPaletteState {
            workspace: PathBuf::from("."),
            source_path: PathBuf::from("config.toml"),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("agent".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };
        let draft = TomlValue::try_from(DiapasonConfig::default()).expect("default config should serialize");

        let items = build_settings_items(&state, &draft).expect("settings items");
        let default_model = items
            .iter()
            .find(|item| item.title == "Default Model")
            .expect("default model entry");
        assert_eq!(
            default_model.selection,
            Some(InlineListSelection::ConfigAction(ACTION_PICK_MAIN_MODEL.to_string()))
        );
    }

    #[test]
    fn ide_context_toggle_action_persists_to_disk() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("config.toml");
        let mut state = SettingsPaletteState {
            workspace: temp.path().to_path_buf(),
            source_path: source_path.clone(),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("ide_context".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };

        apply_settings_action(&mut state, "settings:set:ide_context.enabled:toggle").expect("toggle ide context");

        assert!(!state.draft.ide_context.enabled);
        let persisted = std::fs::read_to_string(&source_path).expect("persisted config");
        assert!(persisted.contains("[ide_context]"));
        assert!(persisted.contains("enabled = false"));
    }

    #[test]
    #[serial]
    fn custom_providers_array_add_uses_valid_template() {
        let temp = tempfile::tempdir().expect("temp dir");
        let user_path = temp.path().join("home").join("config.toml");
        std::fs::create_dir_all(user_path.parent().expect("user config parent")).expect("user config dir");
        let paths = StaticWorkspacePaths::new(temp.path(), temp.path().join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(paths))
            .with_home_paths(vec![user_path.clone()])
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let mut state = SettingsPaletteState {
                workspace: temp.path().to_path_buf(),
                source_path: temp.path().join("config.toml"),
                source_label: "test".to_string(),
                draft: DiapasonConfig::default(),
                view_path: Some("custom_providers".to_string()),
                last_selection: None,
                selection_by_view: BTreeMap::new(),
            };

            let outcome = apply_settings_action(&mut state, "settings:array_add:custom_providers")
                .expect("add custom provider template");

            assert_eq!(outcome.message.as_deref(), Some("Added item to Custom Providers"));
            assert!(outcome.saved);

            assert_eq!(state.draft.custom_providers.len(), 1);
            let provider = &state.draft.custom_providers[0];
            assert_eq!(provider.name, "custom-provider-1");
            assert_eq!(provider.display_name, "Custom Provider 1");
            assert_eq!(provider.base_url, "https://llm.example/v1");
            assert_eq!(provider.api_key_env, "");
            assert_eq!(provider.model, "");

            let persisted = std::fs::read_to_string(&user_path).expect("persisted config");
            assert!(persisted.contains("custom_providers"));
        });
    }

    #[test]
    #[serial]
    fn settings_edit_does_not_copy_trusted_provider_into_workspace_config() {
        let temp = tempfile::tempdir().expect("temp dir");
        let workspace = temp.path();
        let user_path = workspace.join("home").join("config.toml");
        let workspace_path = workspace.join("config.toml");
        std::fs::create_dir_all(user_path.parent().expect("user config parent")).expect("user config dir");
        std::fs::write(
            &user_path,
            r#"
[[custom_providers]]
name = "trusted"
display_name = "Trusted"
base_url = "https://llm.example/v1"
model = "model"
api_key_env = "TRUSTED_API_KEY"
"#,
        )
        .expect("user config");
        std::fs::write(&workspace_path, "agent.provider = \"openai\"\n").expect("workspace config");

        let paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(paths))
            .with_home_paths(vec![user_path.clone()])
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let mut state = create_settings_palette_state(workspace, &None).expect("settings state");
            assert_eq!(
                canonicalize(&state.source_path).expect("canonical source path"),
                canonicalize(&workspace_path).expect("canonical workspace path")
            );
            assert_eq!(state.draft.custom_providers.len(), 1);

            apply_settings_action(&mut state, "settings:set:agent.todo_planning_mode:toggle")
                .expect("workspace setting should persist");

            let workspace_content = std::fs::read_to_string(&workspace_path).expect("workspace config content");
            assert!(workspace_content.contains("todo_planning_mode = false"));
            assert!(!workspace_content.contains("custom_providers"));
            assert!(!workspace_content.contains("trusted"));

            let manager = ConfigManager::load_from_workspace(workspace).expect("reloaded configuration");
            assert_eq!(manager.config().custom_providers.len(), 1);
            assert!(!manager.config().agent.todo_planning_mode);

            apply_settings_action(&mut state, "settings:array_add:custom_providers")
                .expect("custom provider should persist to the trusted layer");
            let workspace_content = std::fs::read_to_string(&workspace_path).expect("workspace config content");
            let user_content = std::fs::read_to_string(&user_path).expect("user config content");
            assert!(!workspace_content.contains("custom_providers"));
            assert!(user_content.contains("custom-provider-1"));

            let manager = ConfigManager::load_from_workspace(workspace).expect("reloaded provider configuration");
            assert_eq!(manager.config().custom_providers.len(), 2);
        });
    }

    #[test]
    fn toggle_action_produces_change_feedback_message() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("config.toml");
        let mut state = SettingsPaletteState {
            workspace: temp.path().to_path_buf(),
            source_path: source_path.clone(),
            source_label: "test".to_string(),
            draft: DiapasonConfig::default(),
            view_path: Some("ide_context".to_string()),
            last_selection: None,
            selection_by_view: BTreeMap::new(),
        };

        let outcome =
            apply_settings_action(&mut state, "settings:set:ide_context.enabled:toggle").expect("toggle ide context");

        assert_eq!(outcome.message.as_deref(), Some("Disabled Ide Context"));
        assert!(outcome.saved);
        assert!(!state.draft.ide_context.enabled);
    }

    #[test]
    fn settings_palette_state_loads_workspace_config_directly() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("config.toml");
        std::fs::write(&source_path, "[workspace]\nuse_root_config = true\n\n[agent]\ntheme = \"ansi\"\n")
            .expect("workspace config should be written");

        let state = create_settings_palette_state(temp.path(), &None).expect("settings state should load");

        assert_eq!(
            canonicalize(&state.source_path).expect("canonical state source path"),
            canonicalize(&source_path).expect("canonical expected source path")
        );
        assert_eq!(state.draft.agent.theme, "ansi");
    }

    #[test]
    #[serial]
    fn settings_reload_is_fail_closed_and_tracks_layer_creation_and_deletion() {
        let temp = tempfile::tempdir().expect("temp dir");
        let workspace = temp.path();
        let fallback_path = workspace.join(diapason_commons::workspace_dir_name()).join("config.toml");
        let paths = StaticWorkspacePaths::new(workspace, workspace.join(diapason_commons::workspace_dir_name()));
        let provider = WorkspacePathsDefaults::new(Arc::new(paths))
            .with_home_paths(Vec::new())
            .with_system_config_paths(Vec::new());

        with_config_defaults_provider_for_test(Arc::new(provider), || {
            let mut state = create_settings_palette_state(workspace, &None).expect("settings state should load");
            let initial_provider = state.draft.agent.provider.clone();

            std::fs::create_dir_all(fallback_path.parent().expect("fallback parent")).expect("fallback dir");
            std::fs::write(&fallback_path, "agent.provider = \"openai\"\n").expect("fallback config");
            reload_state_from_disk(&mut state).expect("created fallback should reload");
            assert_eq!(state.draft.agent.provider, "openai");
            assert_eq!(
                canonicalize(&state.source_path).expect("canonical source path"),
                canonicalize(&fallback_path).expect("canonical fallback path")
            );

            let valid_provider = state.draft.agent.provider.clone();
            std::fs::write(&fallback_path, "agent.provider = [\n").expect("malformed fallback config");
            assert!(reload_state_from_disk(&mut state).is_err());
            assert_eq!(state.draft.agent.provider, valid_provider);

            std::fs::remove_file(&fallback_path).expect("remove fallback config");
            reload_state_from_disk(&mut state).expect("deleted fallback should reload defaults");
            assert_eq!(state.draft.agent.provider, initial_provider);
            assert_eq!(state.source_path, workspace.join("config.toml"));
        });
    }

    #[test]
    fn permission_view_summary_includes_mode_and_rule_counts() {
        let mut config = DiapasonConfig::default();
        config.permissions.allow = vec!["Read".to_string()];
        config.permissions.ask = vec!["Bash".to_string(), "Write".to_string()];
        config.permissions.deny = vec!["Edit".to_string()];

        let summary = format_permission_summary(&config);
        assert!(summary.contains("Rules:"));
        assert!(summary.contains("deny: 1"));
        assert!(summary.contains("ask: 2"));
        assert!(summary.contains("allow: 1"));
    }
}
