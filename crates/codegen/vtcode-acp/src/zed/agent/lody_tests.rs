use chrono::Utc;
use proptest::prelude::*;
use vtcode_core::subagents::{SubagentStatus, SubagentStatusEntry};

use super::lody::owned_child_ids;

fn child(id: &str, session_id: &str, parent_thread_id: &str) -> SubagentStatusEntry {
    let now = Utc::now();
    SubagentStatusEntry {
        id: id.to_string(),
        session_id: session_id.to_string(),
        parent_thread_id: parent_thread_id.to_string(),
        agent_name: "worker".to_string(),
        display_label: id.to_string(),
        description: id.to_string(),
        source: "test".to_string(),
        color: None,
        status: SubagentStatus::Running,
        background: false,
        depth: 1,
        created_at: now,
        updated_at: now,
        completed_at: None,
        summary: None,
        error: None,
        transcript_path: None,
        nickname: None,
    }
}

#[test]
fn ownership_selection_includes_nested_children_and_excludes_foreign_siblings() {
    let entries = vec![
        child("direct", "direct-session", "session-a"),
        child("nested", "nested-session", "direct-session"),
        child("foreign", "foreign-session", "session-b"),
    ];

    let owned = owned_child_ids("session-a", &entries);

    assert_eq!(owned, std::collections::HashSet::from(["direct", "nested"]));
}

proptest! {
    #[test]
    fn ownership_selection_is_the_transitive_session_closure(
        owned_len in 1usize..=16,
        foreign_len in 1usize..=16,
    ) {
        let mut entries = Vec::with_capacity(owned_len + foreign_len);
        let mut parent = "session-a".to_string();
        for index in 0..owned_len {
            let id = format!("owned-{index}");
            let session_id = format!("owned-session-{index}");
            entries.push(child(&id, &session_id, &parent));
            parent = session_id;
        }
        parent = "session-b".to_string();
        for index in 0..foreign_len {
            let id = format!("foreign-{index}");
            let session_id = format!("foreign-session-{index}");
            entries.push(child(&id, &session_id, &parent));
            parent = session_id;
        }

        let owned = owned_child_ids("session-a", &entries);

        prop_assert_eq!(owned.len(), owned_len);
        for index in 0..owned_len {
            let id = format!("owned-{index}");
            prop_assert!(owned.contains(id.as_str()));
        }
        for index in 0..foreign_len {
            let id = format!("foreign-{index}");
            prop_assert!(!owned.contains(id.as_str()));
        }
    }
}
