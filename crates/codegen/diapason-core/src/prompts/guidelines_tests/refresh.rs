//! Covers catalogue refresh property tests.
use super::*;
use proptest::prelude::*;

/// Catalogue section spellings an embedder may still supply.
#[derive(Debug, Clone, Copy)]
enum CatalogueSectionKind {
    /// The current `[Runtime Tool Catalogue]` header.
    Current,
    /// The pre-rename `[Runtime Tool Catalog]` header.
    Legacy,
}

fn unrelated_body_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::char::range('a', 'z'), 1..24).prop_map(|chars| chars.into_iter().collect())
}

fn catalogue_section_strategy() -> impl Strategy<Value = CatalogueSectionKind> {
    proptest::prop_oneof![Just(CatalogueSectionKind::Current), Just(CatalogueSectionKind::Legacy)]
}

fn catalogue_section_text(kind: CatalogueSectionKind, version: u64, epoch: u64) -> String {
    let header = match kind {
        CatalogueSectionKind::Current => "[Runtime Tool Catalogue]",
        CatalogueSectionKind::Legacy => "[Runtime Tool Catalog]",
    };
    format!("{header}\n- version: {version}\n- epoch: {epoch}\n")
}

fn snapshot_with_tool(version: u64, epoch: u64, tool: &str) -> SessionToolCatalogueSnapshot {
    SessionToolCatalogueSnapshot::new(
        version,
        epoch,
        false,
        false,
        Some(std::sync::Arc::new(vec![ToolDefinition::function(
            tool.to_string(),
            "Tool".to_string(),
            serde_json::json!({"type": "object"}),
        )])),
        false,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Refreshing a prompt must drop stale current and legacy catalogue
    /// sections wherever they appear, keep unrelated bracket and Markdown
    /// sections intact, and render the latest snapshot exactly once.
    #[test]
    fn runtime_catalogue_refresh_preserves_unrelated_sections(
        sections in proptest::collection::vec(
            (unrelated_body_strategy(), proptest::option::of(catalogue_section_strategy())),
            2..5,
        ),
        trailing_legacy in proptest::bool::ANY,
        first_version in proptest::num::u64::ANY,
        first_epoch in proptest::num::u64::ANY,
        second_version in proptest::num::u64::ANY,
        second_epoch in proptest::num::u64::ANY,
    ) {
        let mut prompt = String::from("Stable guidance.");
        for (index, (body, catalogue_kind)) in sections.iter().enumerate() {
            if let Some(kind) = catalogue_kind {
                prompt.push('\n');
                prompt.push_str(&catalogue_section_text(*kind, 111 + index as u64, 222 + index as u64));
            }
            if index % 2 == 0 {
                prompt.push_str(&format!("\n[Runtime Marker {index}]\n{body}\n"));
            } else {
                prompt.push_str(&format!("\n## Notes {index}\n{body}\n"));
            }
        }
        if trailing_legacy {
            prompt.push_str("\n[Runtime Tool Catalog]\n- version: 333\n- epoch: 444\n");
        }

        let first = snapshot_with_tool(first_version, first_epoch, TOOL_EXEC_COMMAND);
        let second = snapshot_with_tool(second_version, second_epoch, TOOL_APPLY_PATCH);

        append_runtime_tool_prompt_sections(&mut prompt, &first, true);
        append_runtime_tool_prompt_sections(&mut prompt, &second, true);

        prop_assert_eq!(prompt.matches("## Active Tools").count(), 1);
        prop_assert_eq!(prompt.matches("[Runtime Tool Catalogue]").count(), 1);
        prop_assert_eq!(prompt.matches("- version: ").count(), 1);
        prop_assert!(!prompt.contains("[Runtime Tool Catalog]"));
        prop_assert!(prompt.contains("Stable guidance."));

        for (index, (body, _)) in sections.iter().enumerate() {
            let header = if index % 2 == 0 {
                format!("[Runtime Marker {index}]")
            } else {
                format!("## Notes {index}")
            };
            prop_assert!(prompt.contains(&header), "missing header {header}");
            prop_assert!(prompt.contains(body.as_str()), "missing body {body}");
        }

        let expected_metadata = format!(
            "[Runtime Tool Catalogue]\n- version: {second_version}\n- epoch: {second_epoch}"
        );
        prop_assert!(prompt.contains(&expected_metadata));
    }
}
