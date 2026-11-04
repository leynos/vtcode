import * as vscode from "vscode";
import * as diapasonRunner from "../../utils/diapasonRunner";
import { AnalyseCommand } from "../analyseCommand";

// Mock the diapasonRunner module
jest.mock("../../utils/diapasonRunner");

describe("AnalyseCommand", () => {
    let command: AnalyseCommand;
    let mockContext: any;
    let mockShowInformationMessage: jest.SpiedFunction<
        typeof vscode.window.showInformationMessage
    >;
    let mockShowErrorMessage: jest.SpiedFunction<
        typeof vscode.window.showErrorMessage
    >;

    beforeEach(() => {
        command = new AnalyseCommand();
        mockContext = {
            workspace: undefined,
            activeTextEditor: undefined,
            selection: undefined,
            terminal: undefined,
            output: {
                appendLine: jest.fn(),
            } as any,
        };

        // Mock VS Code API
        mockShowInformationMessage = jest.spyOn(
            vscode.window,
            "showInformationMessage"
        );
        mockShowErrorMessage = jest.spyOn(vscode.window, "showErrorMessage");

        // Reset mocks
        jest.clearAllMocks();
    });

    afterEach(() => {
        jest.restoreAllMocks();
    });

    describe("execute", () => {
        it("should execute analyse command successfully", async () => {
            // Arrange
            (diapasonRunner.runDiapasonCommand as jest.Mock).mockResolvedValue(
                undefined
            );
            mockShowInformationMessage.mockResolvedValue(undefined as any);

            // Act
            await command.execute(mockContext);

            // Assert
            expect(diapasonRunner.runDiapasonCommand).toHaveBeenCalledWith(
                ["analyse"],
                {
                    title: "Analysing workspace with Diapason…",
                    output: mockContext.output,
                }
            );
            expect(mockShowInformationMessage).toHaveBeenCalledWith(
                "Diapason finished analysing the workspace. Review the Diapason output channel for results."
            );
        });

        it("should handle command execution errors", async () => {
            // Arrange
            const mockError = new Error("Analysis failed");
            (diapasonRunner.runDiapasonCommand as jest.Mock).mockRejectedValue(
                mockError
            );
            mockShowErrorMessage.mockResolvedValue(undefined as any);

            // Act
            await command.execute(mockContext);

            // Assert
            expect(diapasonRunner.runDiapasonCommand).toHaveBeenCalled();
            expect(mockShowErrorMessage).toHaveBeenCalledWith(
                "Failed to analyse the workspace with Diapason: Analysis failed"
            );
            expect(mockShowInformationMessage).not.toHaveBeenCalled();
        });

        it("should handle CLI unavailability", async () => {
            // Arrange
            (diapasonRunner.runDiapasonCommand as jest.Mock).mockRejectedValue(
                new Error("CLI not available")
            );
            mockShowErrorMessage.mockResolvedValue(undefined as any);

            // Act
            await command.execute(mockContext);

            // Assert
            expect(mockShowErrorMessage).toHaveBeenCalled();
        });
    });

    describe("command metadata", () => {
        it("should have correct command metadata", () => {
            expect(command.id).toBe("diapason.runAnalyze");
            expect(command.title).toBe("Analyse Workspace");
            expect(command.description).toBe(
                "Analyse the current workspace with Diapason"
            );
            expect(command.icon).toBe("search");
        });
    });

    describe("canExecute", () => {
        it("should return true when CLI is available", async () => {
            // Arrange
            (
                diapasonRunner.ensureCliAvailableForCommand as jest.Mock
            ).mockResolvedValue(true);

            // Act
            const result = await command.canExecute(mockContext);

            // Assert
            expect(result).toBe(true);
        });

        it("should return false when CLI is not available", async () => {
            // Arrange
            (
                diapasonRunner.ensureCliAvailableForCommand as jest.Mock
            ).mockResolvedValue(false);

            // Act
            const result = await command.canExecute(mockContext);

            // Assert
            expect(result).toBe(false);
        });
    });
});
