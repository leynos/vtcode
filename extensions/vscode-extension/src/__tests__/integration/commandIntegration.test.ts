import * as vscode from "vscode";
import { CommandRegistry } from "../../commandRegistry";
import { AnalyseCommand, AskCommand, OpenConfigCommand } from "../../commands";
import * as diapasonRunner from "../../utils/diapasonRunner";

// Mock the diapasonRunner module
jest.mock("../../utils/diapasonRunner");

describe("Command Integration Tests", () => {
    let registry: CommandRegistry;
    let mockContext: any;

    beforeEach(() => {
        registry = new CommandRegistry();
        mockContext = {
            workspace: undefined,
            activeTextEditor: undefined,
            selection: undefined,
            terminal: undefined,
            output: {
                appendLine: jest.fn(),
            } as any,
        };

        // Reset mocks
        jest.clearAllMocks();
    });

    afterEach(() => {
        jest.restoreAllMocks();
    });

    describe("Command Registration and Execution", () => {
        it("should register and execute multiple commands", async () => {
            // Arrange
            const askCommand = new AskCommand();
            const analyseCommand = new AnalyseCommand();
            const openConfigCommand = new OpenConfigCommand();

            registry.registerAll([
                askCommand,
                analyseCommand,
                openConfigCommand,
            ]);

            // Mock command execution
            (diapasonRunner.runDiapasonCommand as jest.Mock).mockResolvedValue(
                undefined
            );
            (
                diapasonRunner.ensureCliAvailableForCommand as jest.Mock
            ).mockResolvedValue(true);
            jest.spyOn(vscode.window, "showInputBox").mockResolvedValue(
                "Test question"
            );
            jest.spyOn(
                vscode.window,
                "showInformationMessage"
            ).mockResolvedValue(undefined as any);

            // Act & Assert
            // Execute ask command
            const askResult = await registry.executeCommand(
                "diapason.askAgent",
                mockContext
            );
            expect(askResult).toBeUndefined();
            expect(diapasonRunner.runDiapasonCommand).toHaveBeenCalledWith(
                ["ask", "Test question"],
                expect.any(Object)
            );

            // Execute analyse command
            const analyseResult = await registry.executeCommand(
                "diapason.runAnalyze",
                mockContext
            );
            expect(analyseResult).toBeUndefined();
            expect(diapasonRunner.runDiapasonCommand).toHaveBeenCalledWith(
                ["analyse"],
                expect.any(Object)
            );

            // Execute open config command
            const openConfigResult = await registry.executeCommand(
                "diapason.openConfig",
                mockContext
            );
            expect(openConfigResult).toBeUndefined();
        });

        it("should handle command execution errors gracefully", async () => {
            // Arrange
            const askCommand = new AskCommand();
            registry.register(askCommand);

            const mockError = new Error("Command execution failed");
            (diapasonRunner.runDiapasonCommand as jest.Mock).mockRejectedValue(
                mockError
            );
            jest.spyOn(vscode.window, "showInputBox").mockResolvedValue(
                "Test question"
            );
            jest.spyOn(vscode.window, "showErrorMessage").mockResolvedValue(
                undefined as any
            );

            // Act
            const result = await registry.executeCommand(
                "diapason.askAgent",
                mockContext
            );

            // Assert
            expect(result).toBeUndefined();
            expect(vscode.window.showErrorMessage).toHaveBeenCalledWith(
                "Failed to ask with Diapason: Command execution failed"
            );
        });

        it("should handle non-existent commands", async () => {
            // Arrange
            const askCommand = new AskCommand();
            registry.register(askCommand);

            // Act
            const result = await registry.executeCommand(
                "diapason.nonExistentCommand",
                mockContext
            );

            // Assert
            expect(result).toBeUndefined();
            expect(vscode.window.showErrorMessage).toHaveBeenCalledWith(
                'Command "diapason.nonExistentCommand" not found'
            );
        });

        it("should check command availability before execution", async () => {
            // Arrange
            const analyseCommand = new AnalyseCommand();
            registry.register(analyseCommand);

            (
                diapasonRunner.ensureCliAvailableForCommand as jest.Mock
            ).mockResolvedValue(false);

            // Act
            const result = await registry.executeCommand(
                "diapason.runAnalyze",
                mockContext
            );

            // Assert
            expect(result).toBe(false);
            expect(diapasonRunner.runDiapasonCommand).not.toHaveBeenCalled();
        });
    });

    describe("Command Registry Management", () => {
        it("should register and unregister commands", () => {
            // Arrange
            const askCommand = new AskCommand();
            const analyseCommand = new AnalyseCommand();

            // Act
            registry.register(askCommand);
            registry.register(analyseCommand);

            // Assert
            expect(registry.getAll()).toHaveLength(2);
            expect(registry.get("diapason.askAgent")).toBe(askCommand);
            expect(registry.get("diapason.runAnalyze")).toBe(analyseCommand);

            // Unregister
            registry.unregister("diapason.askAgent");
            expect(registry.getAll()).toHaveLength(1);
            expect(registry.get("diapason.askAgent")).toBeUndefined();
        });

        it("should clear all registered commands", () => {
            // Arrange
            const askCommand = new AskCommand();
            const analyseCommand = new AnalyseCommand();
            registry.registerAll([askCommand, analyseCommand]);

            // Act
            registry.clear();

            // Assert
            expect(registry.getAll()).toHaveLength(0);
        });

        it("should prevent duplicate command registration", () => {
            // Arrange
            const askCommand1 = new AskCommand();
            const askCommand2 = new AskCommand();

            // Act
            registry.register(askCommand1);
            registry.register(askCommand2);

            // Assert
            expect(registry.getAll()).toHaveLength(1);
            expect(registry.get("diapason.askAgent")).toBe(askCommand2);
        });
    });

    describe("Command Context Handling", () => {
        it("should pass context to commands", async () => {
            // Arrange
            const askCommand = new AskCommand();
            registry.register(askCommand);

            const customContext = {
                ...mockContext,
                workspace: {
                    uri: { fsPath: "/custom/workspace" },
                    name: "Custom Workspace",
                    index: 0,
                } as vscode.WorkspaceFolder,
            };

            (diapasonRunner.runDiapasonCommand as jest.Mock).mockResolvedValue(
                undefined
            );
            jest.spyOn(vscode.window, "showInputBox").mockResolvedValue(
                "Test question"
            );
            jest.spyOn(
                vscode.window,
                "showInformationMessage"
            ).mockResolvedValue(undefined as any);

            // Act
            await registry.executeCommand("diapason.askAgent", customContext);

            // Assert
            expect(diapasonRunner.runDiapasonCommand).toHaveBeenCalledWith(
                ["ask", "Test question"],
                expect.objectContaining({
                    output: customContext.output,
                })
            );
        });
    });
});
