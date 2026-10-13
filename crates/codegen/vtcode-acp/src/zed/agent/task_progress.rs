use super::super::types::SessionHandle;
use super::ZedAgent;
use crate::acp;
use crate::reports::ToolExecutionReport;
use serde_json::{Map, Value, json};
use vtcode_core::config::constants::tools;

const VT_CODE_META_KEY: &str = "vtcode";
const TASK_TRACKER_META_KEY: &str = "taskTracker";
const BLOCKED_TAG: &str = " [blocked]";

pub(super) fn task_plan_from_report(report: &ToolExecutionReport) -> Option<acp::Plan> {
    if report.status != acp::ToolCallStatus::Completed {
        return None;
    }

    task_plan_from_tracker_result(report.raw_output.as_ref()?.get("result")?)
}

fn task_plan_from_tracker_result(result: &Value) -> Option<acp::Plan> {
    let checklist = result.get("checklist")?;
    let checklist_object = checklist.as_object()?;
    let items = checklist_object.get("items")?.as_array()?;
    let entries = items.iter().map(plan_entry_from_item).collect::<Option<Vec<_>>>()?;

    let mut summary = checklist_object.clone();
    let _ = summary.remove("items");

    Some(acp::Plan::new(entries).meta(namespaced_task_meta(Value::Object(summary))))
}

impl ZedAgent {
    /// Re-emits the workspace's persisted tracker as the current ACP plan.
    ///
    /// Reading through the registered tracker tool keeps parsing and
    /// planning-mode persistence in one place. It does not fabricate a
    /// model-visible tool call or pass through interactive permissions.
    pub(super) async fn replay_persisted_task_plan(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
    ) -> anyhow::Result<()> {
        let tracker = session
            .workspace_runtime()
            .and_then(|runtime| runtime.local_tool_registry.get_tool(tools::TASK_TRACKER))
            .or_else(|| self.local_tool_registry.get_tool(tools::TASK_TRACKER));
        let Some(tracker) = tracker else {
            return Ok(());
        };

        let result = tracker.execute(json!({ "action": "list" })).await?;
        if let Some(plan) = task_plan_from_tracker_result(&result) {
            self.send_update(session_id, acp::SessionUpdate::Plan(plan))
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        Ok(())
    }
}

fn plan_entry_from_item(item: &Value) -> Option<acp::PlanEntry> {
    let item_object = item.as_object()?;
    let description = item_object.get("description")?.as_str()?;
    let tracker_status = item_object.get("status")?.as_str()?;
    let (content, status) = map_status(description, tracker_status);

    Some(acp::PlanEntry::new(content, acp::PlanEntryPriority::Medium, status).meta(namespaced_task_meta(item.clone())))
}

fn map_status(description: &str, tracker_status: &str) -> (String, acp::PlanEntryStatus) {
    match tracker_status {
        "in_progress" => (description.to_string(), acp::PlanEntryStatus::InProgress),
        "completed" => (description.to_string(), acp::PlanEntryStatus::Completed),
        "blocked" => (
            format!("{}{BLOCKED_TAG}", description.strip_suffix(BLOCKED_TAG).unwrap_or(description)),
            acp::PlanEntryStatus::Pending,
        ),
        _ => (description.to_string(), acp::PlanEntryStatus::Pending),
    }
}

fn namespaced_task_meta(task_tracker: Value) -> Map<String, Value> {
    let mut vtcode = Map::new();
    let _ = vtcode.insert(TASK_TRACKER_META_KEY.to_string(), task_tracker);

    let mut meta = Map::new();
    let _ = meta.insert(VT_CODE_META_KEY.to_string(), Value::Object(vtcode));
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::ToolExecutionReport;
    use assert_fs::TempDir;
    use proptest::prelude::*;
    use serde_json::json;
    use vtcode_core::tools::handlers::{PlanningWorkflowState, TaskTrackerTool};
    use vtcode_core::tools::traits::Tool;

    fn completed_report(checklist: Value) -> ToolExecutionReport {
        ToolExecutionReport::success(
            Vec::new(),
            Vec::new(),
            json!({
                "status": "success",
                "tool": "task_tracker",
                "result": { "checklist": checklist },
            }),
        )
    }

    #[test]
    fn renders_tracker_summary_and_extended_item_metadata() {
        let report = completed_report(json!({
            "title": "Ship ACP progress",
            "total": 2,
            "completed": 0,
            "in_progress": 1,
            "pending": 0,
            "blocked": 1,
            "progress_percent": 0,
            "notes": "Waiting on review",
            "items": [
                {
                    "index": 1,
                    "description": "Implement rendering",
                    "status": "in_progress",
                    "files": ["src/progress.rs"],
                    "outcome": null,
                    "verify": ["cargo test"]
                },
                {
                    "index_path": "2.1",
                    "level": 1,
                    "description": "Publish release",
                    "status": "blocked",
                    "files": [],
                    "outcome": "Await credentials",
                    "verify": []
                }
            ]
        }));

        let plan = task_plan_from_report(&report).expect("valid task tracker result");
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "Implement rendering");
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::InProgress);
        assert_eq!(plan.entries[1].content, "Publish release [blocked]");
        assert_eq!(plan.entries[1].status, acp::PlanEntryStatus::Pending);
        assert_eq!(
            plan.entries[1].meta.as_ref().and_then(|meta| meta.get("vtcode")),
            Some(&json!({
                "taskTracker": {
                    "index_path": "2.1",
                    "level": 1,
                    "description": "Publish release",
                    "status": "blocked",
                    "files": [],
                    "outcome": "Await credentials",
                    "verify": []
                }
            }))
        );
        assert_eq!(
            plan.meta.as_ref().and_then(|meta| meta.get("vtcode")),
            Some(&json!({
                "taskTracker": {
                    "title": "Ship ACP progress",
                    "total": 2,
                    "completed": 0,
                    "in_progress": 1,
                    "pending": 0,
                    "blocked": 1,
                    "progress_percent": 0,
                    "notes": "Waiting on review"
                }
            }))
        );
    }

    #[test]
    fn ignores_failed_or_malformed_tracker_results() {
        let failed = ToolExecutionReport::failure("task_tracker", "no checklist");
        assert!(task_plan_from_report(&failed).is_none());
        assert!(task_plan_from_report(&completed_report(json!({ "items": [{}] }))).is_none());
    }

    #[tokio::test]
    async fn a_fresh_tracker_instance_rehydrates_the_persisted_plan() {
        let workspace = TempDir::new().expect("tracker workspace");
        let planning = PlanningWorkflowState::new(workspace.path().to_path_buf());
        let first = TaskTrackerTool::new(workspace.path().to_path_buf(), planning.clone());
        let _created = first
            .execute(json!({
                "action": "create",
                "title": "Durable work",
                "items": [
                    { "description": "Resume this task", "status": "in_progress" },
                    { "description": "Wait for input", "status": "blocked" }
                ]
            }))
            .await
            .expect("persist task tracker");

        let fresh = TaskTrackerTool::new(workspace.path().to_path_buf(), planning);
        let result = fresh.execute(json!({ "action": "list" })).await.expect("reload task tracker");
        let plan = task_plan_from_tracker_result(&result).expect("persisted ACP plan");

        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "Resume this task");
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::InProgress);
        assert_eq!(plan.entries[1].content, "Wait for input [blocked]");
    }

    proptest! {
        #[test]
        fn status_mapping_preserves_content_and_exposes_blocked(
            descriptions in prop::collection::vec("[^\\PC]{0,80}", 0..30),
            statuses in prop::collection::vec(
                prop_oneof![
                    Just("pending"),
                    Just("in_progress"),
                    Just("completed"),
                    Just("blocked"),
                ],
                0..30,
            ),
        ) {
            let count = descriptions.len().min(statuses.len());
            let items = descriptions
                .iter()
                .zip(&statuses)
                .take(count)
                .enumerate()
                .map(|(offset, (description, status))| json!({
                    "index": offset + 1,
                    "description": description,
                    "status": status,
                    "files": [],
                    "outcome": null,
                    "verify": [],
                }))
                .collect::<Vec<_>>();
            let plan = task_plan_from_report(&completed_report(json!({
                "title": "Property test",
                "items": items,
            })))
            .expect("generated tracker payload is valid");

            prop_assert_eq!(plan.entries.len(), count);
            for ((entry, description), status) in plan.entries.iter().zip(&descriptions).zip(&statuses).take(count) {
                let expected_content = if *status == "blocked" {
                    format!("{}{BLOCKED_TAG}", description.strip_suffix(BLOCKED_TAG).unwrap_or(description))
                } else {
                    description.clone()
                };
                prop_assert_eq!(&entry.content, &expected_content);
                let expected_status = match *status {
                    "in_progress" => acp::PlanEntryStatus::InProgress,
                    "completed" => acp::PlanEntryStatus::Completed,
                    _ => acp::PlanEntryStatus::Pending,
                };
                prop_assert_eq!(&entry.status, &expected_status);
                prop_assert_eq!(
                    entry
                        .meta
                        .as_ref()
                        .and_then(|meta| meta.get("vtcode"))
                        .and_then(|value| value.get("taskTracker"))
                        .and_then(|value| value.get("status"))
                        .and_then(Value::as_str),
                    Some(*status),
                );
            }
        }
    }
}
