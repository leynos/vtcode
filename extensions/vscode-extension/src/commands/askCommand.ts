import * as vscode from "vscode";
import { BaseCommand, type CommandContext } from "../types/command";
import { runDiapasonCommand } from "../utils/diapasonRunner";

/**
 * Command to ask the Diapason agent a question
 */
export class AskCommand extends BaseCommand {
    public readonly id = "diapason.askAgent";
    public readonly title = "Ask Agent";
    public readonly description = "Ask the Diapason agent a question";
    public readonly icon = "comment-discussion";

    async execute(context: CommandContext): Promise<void> {
        if (!this.ensureCliAvailable(context)) {
            return;
        }

        const question = await vscode.window.showInputBox({
            prompt: "What would you like the Diapason agent to help with?",
            placeHolder: "Summarize src/main.rs",
            ignoreFocusOut: true,
        });

        const trimmedQuestion = question?.trim();
        if (!trimmedQuestion) {
            return;
        }

        try {
            await this.flushIdeContextSnapshot(context);

            await runDiapasonCommand(["ask", trimmedQuestion], {
                title: "Asking Diapason…",
                output: context.output,
            });
            void vscode.window.showInformationMessage(
                "Diapason finished processing your request. Check the Diapason output channel for details."
            );
        } catch (error) {
            this.handleCommandError("ask", error);
        }
    }

    private handleCommandError(context: string, error: unknown): void {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(
            `Failed to ${context} with Diapason: ${message}`
        );
    }
}
