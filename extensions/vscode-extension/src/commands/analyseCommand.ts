import * as vscode from "vscode";
import { BaseCommand, type CommandContext } from "../types/command";
import { runVtcodeCommand } from "../utils/vtcodeRunner";

/**
 * Command to analyse the workspace with VT Code
 */
export class AnalyseCommand extends BaseCommand {
    public readonly id = "vtcode.runAnalyze";
    public readonly title = "Analyse Workspace";
    public readonly description = "Analyse the current workspace with VT Code";
    public readonly icon = "pulse";

    async execute(context: CommandContext): Promise<void> {
        if (!this.ensureCliAvailable(context)) {
            return;
        }

        try {
            await runVtcodeCommand(["analyse"], {
                title: "Analysing workspace with VT Code…",
                output: context.output,
            });
            void vscode.window.showInformationMessage(
                "VT Code finished analysing the workspace. Review the VT Code output channel for results."
            );
        } catch (error) {
            this.handleCommandError("analyse the workspace", error);
        }
    }

    private handleCommandError(context: string, error: unknown): void {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(
            `Failed to ${context} with VT Code: ${message}`
        );
    }
}
