import * as vscode from "vscode";
import { BaseCommand, type CommandContext } from "../types/command";
import { pickDiapasonConfigUri } from "../diapasonConfig";

/**
 * Command to open the Diapason configuration file
 */
export class OpenConfigCommand extends BaseCommand {
    public readonly id = "diapason.openConfig";
    public readonly title = "Open Configuration";
    public readonly description = "Open the config.toml configuration file";
    public readonly icon = "gear";

    async execute(context: CommandContext): Promise<void> {
        try {
            const configUri = await pickDiapasonConfigUri();
            if (!configUri) {
                void vscode.window.showWarningMessage(
                    "No config.toml file was found in this workspace."
                );
                return;
            }

            const document = await vscode.workspace.openTextDocument(configUri);
            await vscode.window.showTextDocument(document, {
                preview: false,
            });
        } catch (error) {
            this.handleCommandError("open configuration", error);
        }
    }

    private handleCommandError(context: string, error: unknown): void {
        const message = error instanceof Error ? error.message : String(error);
        void vscode.window.showErrorMessage(
            `Failed to ${context} with Diapason: ${message}`
        );
    }
}
