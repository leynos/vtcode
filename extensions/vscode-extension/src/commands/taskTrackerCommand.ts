import * as vscode from "vscode";
import { BaseCommand, type CommandContext } from "../types/command";
import { type DiapasonTaskDefinition } from "../utils/diapasonRunner";

const TASK_TRACKER_COMMAND = "task-tracker";
const LEGACY_UPDATE_PLAN_COMMAND = "update-plan";

function hasLegacyUpdatePlanTask(tasks: vscode.Task[]): boolean {
    return tasks.some(
        (task) =>
            (task.definition as { command?: unknown }).command ===
            LEGACY_UPDATE_PLAN_COMMAND
    );
}

/**
 * Command to run the Diapason task tracker
 */
export class TaskTrackerCommand extends BaseCommand {
    public readonly id = "diapason.runTaskTrackerTask";
    public readonly title = "Task Tracker";
    public readonly description = "Run the Diapason task tracker";
    public readonly icon = "checklist";

    async execute(context: CommandContext): Promise<void> {
        if (!this.ensureCliAvailable(context)) {
            return;
        }

        const tasks = await vscode.tasks.fetchTasks({ type: "diapason" });
        const taskTrackerTasks = tasks.filter(
            (task) =>
                (task.definition as DiapasonTaskDefinition).command ===
                TASK_TRACKER_COMMAND
        );

        if (taskTrackerTasks.length === 0) {
            const warningMessage = hasLegacyUpdatePlanTask(tasks)
                ? 'No Diapason task tracker tasks are available. Migrate tasks.json entries from command "update-plan" to "task-tracker".'
                : "No Diapason task tracker tasks are available. Define a Diapason task in tasks.json to customize the workflow.";
            void vscode.window.showWarningMessage(
                warningMessage
            );
            return;
        }

        let taskToRun: vscode.Task | undefined;
        if (taskTrackerTasks.length === 1) {
            taskToRun = taskTrackerTasks[0];
        } else {
            const pickItems = taskTrackerTasks.map((task) => ({
                label: task.name,
                task,
            }));
            const selection = await vscode.window.showQuickPick(pickItems, {
                placeHolder: "Select the Diapason task tracker task to run",
            });
            taskToRun = selection?.task;
        }

        if (!taskToRun) {
            return;
        }

        await this.flushIdeContextSnapshot(context);
        await vscode.tasks.executeTask(taskToRun);
    }
}
