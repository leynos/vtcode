import * as vscode from "vscode";
import { BaseCommand, type CommandContext } from "../types/command";
import { runDiapasonCommand } from "../utils/diapasonRunner";

/**
 * Command to analyse the workspace with Diapason
 */
export class AnalyseCommand extends BaseCommand {
    public readonly id = "diapason.runAnalyze";
    public readonly title = "Analyse Workspace";
    public readonly description = "Analyse the current workspace with Diapason";
    public readonly icon = "pulse";

    async execute(context: CommandContext): Promise<void> {
        if (!this.ensureCliAvailable(context)) {
            return;
        }

        try {
            await runDiapasonCommand(["analyse"], {
                title: "Analysing workspace with Diapason…",
                output: context.output,
            });
            void vscode.window.showInformationMessage(
                "Diapason finished analysing the workspace. Review the Diapason output channel for results."
            );
        } catch (error) {
            this.handleCommandError("analyse the workspace", error);
        }
    }

    private handleCommandError(context: string, error: unknown): void {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(
            `Failed to ${context} with Diapason: ${message}`
        );
    }
}
