//! Replan and augment helpers for the plan-build-evaluate harness.
//!
//! Contains methods for replanning after evaluator rejection and augmenting
//! generator tasks with contract information.

use super::AgentRunner;
use super::evaluator_types::GeneralizationNote;
use super::orchestration::{EvaluationArtefacts, PlannerArtefacts};
use super::planner_types::ReplanResponse;
use crate::core::agent::harness_artefacts;
use crate::core::agent::task::Task;
use crate::tools::handlers::TaskTrackerTool;
use crate::tools::traits::Tool;
use serde_json::json;
use tracing::warn;

impl AgentRunner {
    /// Build a `TaskTrackerTool` from the runner's workspace and planning
    /// workflow state. Extracted to avoid repeating construction in
    /// `apply_required_tracker_updates` and `replan_from_failure`.
    fn tracker_tool(&self) -> TaskTrackerTool {
        TaskTrackerTool::new(self._workspace.clone(), self.tool_registry.planning_workflow_state())
    }

    /// Re-plan from the current state after an evaluator rejection.
    ///
    /// Following the long-running harness pattern: "the evaluator takes on part
    /// of the local planner role for feedback-driven replanning." This method:
    ///
    /// 1. Attempts an LLM-based structured replan (`request_replan_response`)
    ///    that produces a revised feature list, contract addendum, and new
    ///    tracker items.
    /// 2. Applies the evaluator's `required_tracker_updates` to the tracker.
    /// 3. Falls back to annotation-only (appending evaluator feedback to
    ///    spec/contract/feature-list) if the LLM replan fails.
    pub(super) async fn replan_from_failure(
        &mut self,
        task: &Task,
        evaluation: &EvaluationArtefacts,
        revision_round: usize,
    ) -> Option<PlannerArtefacts> {
        let spec_path = harness_artefacts::current_spec_path(&self._workspace);
        let contract_path = harness_artefacts::current_contract_path(&self._workspace);
        let tracker_path = harness_artefacts::current_task_path(&self._workspace);
        let feature_list_path = harness_artefacts::current_feature_list_path(&self._workspace);

        // Apply evaluator's required tracker updates to the tracker tool.
        if !evaluation.required_tracker_updates.is_empty() {
            self.apply_required_tracker_updates(&evaluation.required_tracker_updates).await;
        }

        self.apply_generalization_falsifiers(&evaluation.generalization_notes).await;

        // Attempt LLM-based structured replan.
        let replan = self
            .request_replan_response(task, evaluation, revision_round)
            .await
            .filter(|response| preserves_generalization_scopes(response, &evaluation.generalization_notes));

        if evaluation.generalization_notes.iter().any(|note| {
            replan
                .as_ref()
                .is_none_or(|response| !preserves_generalization_scopes(response, std::slice::from_ref(note)))
        }) {
            warn!("replanner response did not preserve every generalization-note scope; retaining the prior plan");
        }

        if let Some(ref replan) = replan {
            self.apply_replan_response(replan).await;
        } else {
            // Fallback: annotate artefacts with evaluator feedback.
            for (label, path) in [
                ("spec", &spec_path),
                ("contract", &contract_path),
                ("feature_list", &feature_list_path),
            ] {
                annotate_artefact(&self._workspace, path, label, &evaluation.summary, revision_round).await;
            }
        }

        self.append_generalization_scope_contract(&evaluation.generalization_notes, revision_round)
            .await;

        Some(PlannerArtefacts {
            spec_path,
            contract_path,
            tracker_path,
            feature_list_path,
        })
    }

    /// Apply the evaluator's `required_tracker_updates` by adding each as a
    /// new tracker item.
    async fn apply_required_tracker_updates(&self, updates: &[String]) {
        let tracker_tool = self.tracker_tool();
        for update in updates {
            let trimmed = update.trim();
            if trimmed.is_empty() {
                continue;
            }
            let result = tracker_tool
                .execute(json!({
                    "action": "add",
                    "item": trimmed,
                }))
                .await;
            if let Err(e) = result {
                warn!(error = %e, item = trimmed, "failed to add required tracker update");
            }
        }
    }

    async fn apply_generalization_falsifiers(&self, notes: &[GeneralizationNote]) {
        if notes.is_empty() {
            return;
        }

        let tracker_tool = self.tracker_tool();
        for note in notes {
            if let Err(error) = tracker_tool
                .execute(json!({
                    "action": "add",
                    "description": format!("Falsify task-scoped claim: {}", note.claim),
                    "outcome": format!("The claim is tested only within scope: {}", note.scope),
                    "verify": [note.falsifier],
                }))
                .await
            {
                warn!(error = %error, "failed to add generalization falsifier to tracker");
            }
        }
    }

    async fn append_generalization_scope_contract(&self, notes: &[GeneralizationNote], revision_round: usize) {
        if notes.is_empty() {
            return;
        }

        let contract_path = harness_artefacts::current_contract_path(&self._workspace);
        let existing = tokio::fs::read_to_string(&contract_path).await.unwrap_or_default();
        let mut guardrails =
            format!("\n\n--- Evidence-Bounded Generalization Guardrails (round {revision_round}) ---\n");
        for note in notes {
            guardrails.push_str(&format!(
                "- Scope: {}\n  Evidence: {}\n  Falsifier: {}\n",
                note.scope, note.evidence, note.falsifier
            ));
        }
        let updated = format!("{existing}{guardrails}");
        if let Err(error) = harness_artefacts::write_contract(&self._workspace, &updated).await {
            warn!(error = %error, "failed to preserve generalization-note scopes in contract");
        }
    }

    /// Apply a structured replan response: overwrite the feature list, append
    /// the contract addendum, and add new tracker items.
    async fn apply_replan_response(&self, replan: &ReplanResponse) {
        if !replan.rationale.is_empty() {
            tracing::info!(
                rationale = %replan.rationale,
                "Applying structured replan from evaluator feedback"
            );
        }
        if let Some(ref feature_list) = replan.revised_feature_list {
            let trimmed = feature_list.trim();
            if !trimmed.is_empty() {
                if let Err(e) = harness_artefacts::write_feature_list(&self._workspace, trimmed).await {
                    warn!(error = %e, "failed to write revised feature list");
                }
            }
        }

        if let Some(ref addendum) = replan.contract_addendum {
            let trimmed = addendum.trim();
            if !trimmed.is_empty() {
                let contract_path = harness_artefacts::current_contract_path(&self._workspace);
                let existing = tokio::fs::read_to_string(&contract_path).await.unwrap_or_default();
                let updated = format!("{existing}\n\n--- Replan Addendum ---\n{trimmed}\n");
                if let Err(e) = harness_artefacts::write_contract(&self._workspace, &updated).await {
                    warn!(error = %e, "failed to write contract addendum");
                }
            }
        }

        if !replan.new_tracker_items.is_empty() {
            let tracker_tool = self.tracker_tool();
            let items: Vec<serde_json::Value> = replan
                .new_tracker_items
                .iter()
                .map(|item| {
                    json!({
                        "description": item.description,
                        "outcome": item.outcome,
                        "verify": item.verify,
                        "files": item.files,
                    })
                })
                .collect();
            if let Err(e) = tracker_tool
                .execute(json!({
                    "action": "add_items",
                    "items": items,
                }))
                .await
            {
                warn!(error = %e, "failed to add new tracker items from replan");
            }
        }
    }

    /// Augment a task with generator contract instructions.
    pub(super) fn augment_generator_task(&self, task: &Task, artefacts: &PlannerArtefacts) -> Task {
        let mut effective_task = task.clone();
        let addendum = format!(
            "Generator contract:\n- Treat `{}`, `{}`, `{}`, and `{}` as the source of truth.\n- The execution contract defines what done must look like in observable terms.\n- The feature list enumerates the project's features with acceptance criteria.\n- Work one tracker step at a time.\n- Do not mark a step done until the implementation and verification evidence both support it.\n- Keep the tracker current.\n- Leave resumable state before yielding.",
            artefacts.spec_path.display(),
            artefacts.contract_path.display(),
            artefacts.feature_list_path.display(),
            artefacts.tracker_path.display()
        );
        effective_task.instructions = Some(match task.instructions.as_deref() {
            Some(existing) if !existing.trim().is_empty() => format!("{existing}\n\n{addendum}"),
            _ => addendum,
        });
        effective_task
    }
}

fn preserves_generalization_scopes(response: &ReplanResponse, notes: &[GeneralizationNote]) -> bool {
    notes
        .iter()
        .all(|note| response.preserved_scopes.iter().any(|scope| scope.trim() == note.scope))
}

async fn annotate_artefact(
    workspace: &std::path::Path,
    path: &std::path::Path,
    label: &str,
    evaluation_summary: &str,
    revision_round: usize,
) {
    let existing = tokio::fs::read_to_string(path).await.unwrap_or_default();
    let annotated = format!(
        "{existing}\n\n\
         --- Revision Round {revision_round} ---\n\
         Evaluator feedback:\n{evaluation_summary}\n",
    );
    let write_fn = match label {
        "spec" => harness_artefacts::write_spec(workspace, &annotated).await,
        "feature_list" => harness_artefacts::write_feature_list(workspace, &annotated).await,
        _ => harness_artefacts::write_contract(workspace, &annotated).await,
    };
    let _ = write_fn.inspect_err(|e| warn!(error = %e, "annotate_artefact: failed to annotate {label}"));
}
