import * as vscode from "vscode";
import { BaseCommand, type CommandContext } from "../types/command";
import { runDiapasonCommand } from "../utils/diapasonRunner";

/**
 * Command to list all files in the workspace
 *
 * Enumerates all discoverable files with optional exclusion patterns.
 * Respects .gitignore files by default.
 */
export class ListFilesCommand extends BaseCommand {
    public readonly id = "diapason.listFiles";
    public readonly title = "List Files";
    public readonly description = "List all files in the workspace";
    public readonly icon = "list-flat";

    async execute(context: CommandContext): Promise<void> {
        if (!this.ensureCliAvailable(context)) {
            return;
        }

        const excludeStr = await vscode.window.showInputBox({
            prompt: "Enter exclusion patterns (comma-separated, e.g., 'target/**,node_modules/**')",
            placeHolder: "Leave empty to include all files",
            ignoreFocusOut: true,
        });

        try {
            const args = ["list-files"];
            if (excludeStr && excludeStr.trim()) {
                args.push("--exclude", excludeStr.trim());
            }

            await runDiapasonCommand(args, {
                title: "Listing workspace files…",
                output: context.output,
            });

            void vscode.window.showInformationMessage(
                "File listing completed. Check the Diapason output channel for results."
            );
        } catch (error) {
            this.handleCommandError("list files", error);
        }
    }

    private handleCommandError(context: string, error: unknown): void {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(
            `Failed to ${context}: ${message}`
        );
    }
}
