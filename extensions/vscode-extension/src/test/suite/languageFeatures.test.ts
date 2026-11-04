import * as assert from "assert";
import * as vscode from "vscode";

suite("Diapason configuration language features", () => {
    suiteSetup(async () => {
        const extension = vscode.extensions.getExtension(
            "df12productions.diapason-companion"
        );
        assert.ok(extension, "Diapason Companion extension not found");
        await extension.activate();
    });

    test("provides completions for agent keys", async () => {
        const document = await vscode.workspace.openTextDocument({
            language: "toml",
            content: "[agent]\n",
        });
        const diapasonDocument = await vscode.languages.setTextDocumentLanguage(
            document,
            "diapason-config"
        );
        await vscode.window.showTextDocument(diapasonDocument);

        const position = new vscode.Position(1, 0);
        const completionList =
            await vscode.commands.executeCommand<vscode.CompletionList>(
                "vscode.executeCompletionItemProvider",
                diapasonDocument.uri,
                position
            );

        assert.ok(completionList, "Expected completions for config.toml");
        const labels = completionList?.items.map((item) =>
            typeof item.label === "string" ? item.label : item.label.label
        );
        assert.ok(
            labels?.includes("provider"),
            "Expected provider completion in agent section"
        );
    });

    test("shows hover text for agent section headers", async () => {
        const document = await vscode.workspace.openTextDocument({
            language: "toml",
            content: "[agent]\n",
        });
        const diapasonDocument = await vscode.languages.setTextDocumentLanguage(
            document,
            "diapason-config"
        );

        const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
            "vscode.executeHoverProvider",
            diapasonDocument.uri,
            new vscode.Position(0, 1)
        );

        assert.ok(
            hovers && hovers.length > 0,
            "Expected hover information for agent section"
        );
        const hoverText = hovers
            .flatMap((hover) => hover.contents)
            .map((content) =>
                typeof content === "string" ? content : content.value
            )
            .join("\n");
        assert.ok(
            hoverText.includes("Core Diapason agent behaviour"),
            "Hover text should describe agent section"
        );
    });

    test("creates document symbols for configuration sections", async () => {
        const document = await vscode.workspace.openTextDocument({
            language: "toml",
            content:
                '[agent]\nprovider = "openai"\n\n[agent.onboarding]\nenabled = true\n',
        });
        const diapasonDocument = await vscode.languages.setTextDocumentLanguage(
            document,
            "diapason-config"
        );

        const symbols = await vscode.commands.executeCommand<
            vscode.DocumentSymbol[]
        >("vscode.executeDocumentSymbolProvider", diapasonDocument.uri);

        assert.ok(
            symbols && symbols.length > 0,
            "Expected document symbols for config.toml"
        );
        const symbolNames = flattenSymbols(symbols).map(
            (symbol) => symbol.name
        );
        assert.ok(
            symbolNames.includes("Agent"),
            "Document symbols should include the Agent section"
        );
        assert.ok(
            symbolNames.includes("Onboarding"),
            "Document symbols should include nested sections"
        );
    });
});

function flattenSymbols(
    symbols: vscode.DocumentSymbol[]
): vscode.DocumentSymbol[] {
    return symbols.flatMap((symbol) => [
        symbol,
        ...flattenSymbols(symbol.children),
    ]);
}
