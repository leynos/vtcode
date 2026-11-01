//! Exercises the ownership-selection boundary for Lody subagent management.

use std::collections::HashSet;

use chrono::Utc;
use proptest::prelude::*;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentStatus, SubagentStatusEntry,
};

use super::lody::{owned_background_ids, owned_child_ids};

/// Upper bound from the `SESSION-ISOLATION` verification domain.
const MAX_FOREST_TASKS: usize = 32;
/// Session roots per generated forest: the requested root plus one to three
/// foreign roots.
const SESSION_ROOT_PREFIX: &str = "session-root-";

fn child(id: &str, session_id: &str, parent_thread_id: &str) -> SubagentStatusEntry {
    forest_node(id, session_id, parent_thread_id, false, SubagentStatus::Running)
}

fn forest_node(
    id: &str,
    session_id: &str,
    parent_thread_id: &str,
    background: bool,
    status: SubagentStatus,
) -> SubagentStatusEntry {
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
        status,
        background,
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

/// One generated forest node. `attach` selects the parent once the node has a
/// predecessor; `root` is the session root used when the node attaches at the
/// root. `shuffle_key` orders the emitted entry list.
#[derive(Debug, Clone)]
struct ForestNodeSpec {
    attach: usize,
    root: usize,
    background: bool,
    status: SubagentStatus,
    shuffle_key: u64,
}

fn status_strategy() -> impl Strategy<Value = SubagentStatus> {
    prop_oneof![
        Just(SubagentStatus::Queued),
        Just(SubagentStatus::Running),
        Just(SubagentStatus::Waiting),
        Just(SubagentStatus::Completed),
        Just(SubagentStatus::Failed),
        Just(SubagentStatus::Closed),
    ]
}

fn forest_node_spec() -> impl Strategy<Value = ForestNodeSpec> {
    (0usize..MAX_FOREST_TASKS, 0usize..4, any::<bool>(), status_strategy(), any::<u64>()).prop_map(
        |(attach, root, background, status, shuffle_key)| ForestNodeSpec {
            attach,
            root,
            background,
            status,
            shuffle_key,
        },
    )
}

#[test]
fn ownership_selection_includes_nested_children_and_excludes_foreign_siblings() {
    let entries = vec![
        child("direct", "direct-session", "session-a"),
        child("nested", "nested-session", "direct-session"),
        child("foreign", "foreign-session", "session-b"),
    ];

    let owned = owned_child_ids("session-a", &entries);

    assert_eq!(owned, HashSet::from(["direct", "nested"]));
}

proptest! {
    /// The generated domain is an acyclic mixed forest: the requested root,
    /// one to three foreign roots, foreground and background child entries,
    /// and every status. Node 0 is a direct child, node 1 a nested descendant
    /// and node 2 a foreign child, so every generated case has the witnesses
    /// the verification plan requires. This property covers foreground
    /// controller ancestry, including the entry's own `background` flag;
    /// managed background records have a separate ownership path and their own
    /// property below.
    #[test]
    fn ownership_selection_is_the_transitive_session_closure_of_a_mixed_forest(
        root_count in 2usize..=4,
        specs in prop::collection::vec(forest_node_spec(), 3..=MAX_FOREST_TASKS),
    ) {
        let root_session = format!("{SESSION_ROOT_PREFIX}0");
        let mut entries: Vec<SubagentStatusEntry> = Vec::with_capacity(specs.len());
        let mut session_ids: Vec<String> = Vec::with_capacity(specs.len());
        let mut root_of: Vec<usize> = Vec::with_capacity(specs.len());

        for (index, spec) in specs.iter().enumerate() {
            let (parent_thread_id, root_index) = match index {
                0 => (root_session.clone(), 0),
                1 => (session_ids[0].clone(), root_of[0]),
                2 => {
                    let root = 1 + spec.root % (root_count - 1);
                    (format!("{SESSION_ROOT_PREFIX}{root}"), root)
                }
                _ => {
                    let attach = spec.attach % (index + 1);
                    if attach == index {
                        let root = spec.root % root_count;
                        (format!("{SESSION_ROOT_PREFIX}{root}"), root)
                    } else {
                        (session_ids[attach].clone(), root_of[attach])
                    }
                }
            };
            let id = format!("task-{index}");
            let session_id = format!("node-session-{index}");
            // Node 0 is a direct child and is always background, so every
            // generated case carries the plan's background witness instead of
            // depending on `any::<bool>()` to produce one.
            let background = index == 0 || spec.background;
            entries.push(forest_node(&id, &session_id, &parent_thread_id, background, spec.status));
            session_ids.push(session_id);
            root_of.push(root_index);
        }

        // Delivery order is arbitrary: the ownership closure must not depend on
        // it, so compare the shuffled answer with the generation-order answer.
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by_key(|&index| specs[index].shuffle_key);
        let shuffled: Vec<SubagentStatusEntry> =
            order.iter().map(|&index| entries[index].clone()).collect();

        let owned = owned_child_ids(&root_session, &shuffled);
        let owned_in_generation_order = owned_child_ids(&root_session, &entries);
        prop_assert_eq!(owned.clone(), owned_in_generation_order);

        let expected: HashSet<&str> = entries
            .iter()
            .enumerate()
            .filter(|(index, _)| root_of[*index] == 0)
            .map(|(_, entry)| entry.id.as_str())
            .collect();
        prop_assert_eq!(&owned, &expected);

        for (index, entry) in entries.iter().enumerate() {
            if root_of[index] == 0 {
                prop_assert!(owned.contains(entry.id.as_str()));
            } else {
                prop_assert!(
                    !owned.contains(entry.id.as_str()),
                    "a task in another session root must stay foreign: {}",
                    entry.id
                );
            }
        }

        // Negative control: a naive filter that keeps only direct children is
        // rejected by the generated nested witness.
        let direct_only: HashSet<&str> = shuffled
            .iter()
            .filter(|entry| entry.parent_thread_id == root_session)
            .map(|entry| entry.id.as_str())
            .collect();
        prop_assert!(direct_only.is_subset(&owned));
        prop_assert!(owned.contains("task-1"), "the generator must yield a nested owned witness");
        prop_assert!(
            !direct_only.contains("task-1"),
            "a direct-child-only filter must miss the nested witness"
        );
        prop_assert_ne!(direct_only, owned);
    }
}

fn background_status_strategy() -> impl Strategy<Value = BackgroundSubprocessStatus> {
    prop_oneof![
        Just(BackgroundSubprocessStatus::Starting),
        Just(BackgroundSubprocessStatus::Running),
        Just(BackgroundSubprocessStatus::Stopped),
        Just(BackgroundSubprocessStatus::Error),
    ]
}

/// One generated managed background record. `owner` selects the requested
/// session (0), a foreign session root (1) or no owner at all (2); when the
/// second is set the field carries the string `None`.
#[derive(Debug, Clone)]
struct BackgroundRecordSpec {
    owner: usize,
    status: BackgroundSubprocessStatus,
    shuffle_key: u64,
}

fn background_record_spec() -> impl Strategy<Value = BackgroundRecordSpec> {
    (0usize..3, background_status_strategy(), any::<u64>())
        .prop_map(|(owner, status, shuffle_key)| BackgroundRecordSpec { owner, status, shuffle_key })
}

fn background_entry(
    id: &str,
    owner_session_id: Option<&str>,
    status: BackgroundSubprocessStatus,
) -> BackgroundSubprocessEntry {
    let now = Utc::now();
    BackgroundSubprocessEntry {
        id: id.to_string(),
        owner_session_id: owner_session_id.map(str::to_owned),
        session_id: format!("background-session-{id}"),
        exec_session_id: format!("exec-{id}"),
        agent_name: "watcher".to_string(),
        display_label: id.to_string(),
        description: id.to_string(),
        source: "test".to_string(),
        color: None,
        status,
        desired_enabled: true,
        created_at: now,
        updated_at: now,
        started_at: None,
        ended_at: None,
        pid: None,
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
    }
}

proptest! {
    /// Managed background records are owned through their explicit
    /// `owner_session_id`, not through foreground controller ancestry. The
    /// generated domain mixes records owned by the requested session, by a
    /// foreign root and by no session at all, with every status and shuffled
    /// delivery order. Records 0, 1 and 2 force one witness of each kind.
    #[test]
    fn background_ownership_is_the_owner_session_field(
        specs in prop::collection::vec(background_record_spec(), 3..=MAX_FOREST_TASKS),
    ) {
        let root_session = format!("{SESSION_ROOT_PREFIX}0");
        let foreign_session = format!("{SESSION_ROOT_PREFIX}1");
        let owners: Vec<usize> = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| match index {
                0 => 0,
                1 => 1,
                2 => 2,
                _ => spec.owner,
            })
            .collect();
        let entries: Vec<BackgroundSubprocessEntry> = owners
            .iter()
            .enumerate()
            .map(|(index, &owner)| {
                let owner_session_id = match owner {
                    0 => Some(root_session.as_str()),
                    1 => Some(foreign_session.as_str()),
                    _ => None,
                };
                background_entry(&format!("background-{index}"), owner_session_id, specs[index].status)
            })
            .collect();

        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by_key(|&index| specs[index].shuffle_key);
        let shuffled: Vec<BackgroundSubprocessEntry> =
            order.iter().map(|&index| entries[index].clone()).collect();

        let owned = owned_background_ids(&root_session, &shuffled);
        prop_assert_eq!(owned.clone(), owned_background_ids(&root_session, &entries));

        let expected: HashSet<&str> = entries
            .iter()
            .enumerate()
            .filter(|(index, _)| owners[*index] == 0)
            .map(|(_, entry)| entry.id.as_str())
            .collect();
        prop_assert_eq!(&owned, &expected);

        for (index, entry) in entries.iter().enumerate() {
            if owners[index] == 0 {
                prop_assert!(owned.contains(entry.id.as_str()));
            } else {
                prop_assert!(
                    !owned.contains(entry.id.as_str()),
                    "a background record owned by another session or by no session must stay foreign: {}",
                    entry.id
                );
            }
        }

        // Negative control: an owner-agnostic filter keeps every record and is
        // rejected by the generated foreign and ownerless witnesses.
        let owner_agnostic: HashSet<&str> = shuffled.iter().map(|entry| entry.id.as_str()).collect();
        prop_assert!(owners.contains(&0));
        prop_assert!(owners.contains(&1));
        prop_assert!(owners.contains(&2));
        prop_assert!(owned.is_subset(&owner_agnostic));
        prop_assert_ne!(owner_agnostic, owned);
    }
}
