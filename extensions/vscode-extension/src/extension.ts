import { spawn, type SpawnOptionsWithoutStdio } from "node:child_process";
import * as vscode from "vscode";
import { CommandRegistry } from "./commandRegistry";
import {
    AnalyseCommand,
    AskCommand,
    AskSelectionCommand,
    FindFilesCommand,
    ListFilesCommand,
    OpenConfigCommand,
    RefreshCommand,
    SearchFilesCommand,
    TrustWorkspaceCommand,
    TaskTrackerCommand,
} from "./commands";
import { registerDiapasonLanguageFeatures } from "./languageFeatures";
import {
    appendMcpProvider,
    loadConfigSummaryFromUri,
    pickDiapasonConfigUri,
    registerDiapasonConfigWatcher,
    revealMcpSection,
    revealToolsPolicySection,
    setHumanInTheLoop,
    setMcpProviderEnabled,
    DiapasonConfigSummary,
} from "./diapasonConfig";

type QuickActionItem = vscode.QuickPickItem & {
    run: () => Thenable<unknown> | void;
};

interface QuickActionDescription {
    readonly label: string;
    readonly description: string;
    readonly command: string;
    readonly icon?: string;
    readonly args?: unknown[];
}

class QuickActionTreeItem extends vscode.TreeItem {
    constructor(public readonly action: QuickActionDescription) {
        super(action.label, vscode.TreeItemCollapsibleState.None);
        this.description = action.description;
        this.iconPath = new vscode.ThemeIcon(action.icon ?? "rocket");
        this.command = {
            command: action.command,
            title: action.label,
            arguments: action.args,
        };
        this.contextValue = "diapasonQuickAction";
    }
}

class QuickActionTreeDataProvider
    implements vscode.TreeDataProvider<QuickActionTreeItem>
{
    private readonly onDidChangeTreeDataEmitter =
        new vscode.EventEmitter<void>();
    readonly onDidChangeTreeData = this.onDidChangeTreeDataEmitter.event;

    constructor(private readonly getActions: () => QuickActionDescription[]) {}

    getTreeItem(element: QuickActionTreeItem): vscode.TreeItem {
        return element;
    }

    getChildren(): vscode.ProviderResult<QuickActionTreeItem[]> {
        return this.getActions().map(
            (action) => new QuickActionTreeItem(action)
        );
    }

    refresh(): void {
        this.onDidChangeTreeDataEmitter.fire();
    }
}

interface WorkspaceInsightDescription {
    readonly label: string;
    readonly description: string;
    readonly icon: string;
    readonly command?: vscode.Command;
    readonly tooltip?: string | vscode.MarkdownString;
}

class WorkspaceInsightTreeItem extends vscode.TreeItem {
    constructor(public readonly insight: WorkspaceInsightDescription) {
        super(insight.label, vscode.TreeItemCollapsibleState.None);
        this.description = insight.description;
        this.iconPath = new vscode.ThemeIcon(insight.icon);
        this.command = insight.command;
        if (insight.tooltip) {
            this.tooltip = insight.tooltip;
        }
        this.contextValue = "diapasonWorkspaceInsight";
    }
}

class WorkspaceInsightsTreeDataProvider
    implements vscode.TreeDataProvider<WorkspaceInsightTreeItem>
{
    private readonly onDidChangeTreeDataEmitter =
        new vscode.EventEmitter<void>();
    readonly onDidChangeTreeData = this.onDidChangeTreeDataEmitter.event;

    constructor(
        private readonly getInsights: () => WorkspaceInsightDescription[]
    ) {}

    getTreeItem(element: WorkspaceInsightTreeItem): vscode.TreeItem {
        return element;
    }

    getChildren(): vscode.ProviderResult<WorkspaceInsightTreeItem[]> {
        return this.getInsights().map(
            (insight) => new WorkspaceInsightTreeItem(insight)
        );
    }

    refresh(): void {
        this.onDidChangeTreeDataEmitter.fire();
    }
}

let outputChannel: vscode.OutputChannel | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;
let quickActionsProviderInstance: QuickActionTreeDataProvider | undefined;
let workspaceInsightsProvider: WorkspaceInsightsTreeDataProvider | undefined;
let agentTerminal: vscode.Terminal | undefined;
let terminalCloseListener: vscode.Disposable | undefined;
let cliAvailable = false;
let missingCliWarningShown = false;
let cliAvailabilityCheck: Promise<void> | undefined;
let currentConfigSummary: DiapasonConfigSummary | undefined;
let lastConfigUri: string | undefined;
let lastConfigParseError: string | undefined;
let lastAutomationFullAutoEnabled: boolean | undefined;
let workspaceTrusted = vscode.workspace.isTrusted;
let activationContext: vscode.ExtensionContext | undefined;
let lastProviderModelWarningKey: string | undefined;

const CLI_DETECTION_TIMEOUT_MS = 4000;
const DIAPASON_CHAT_PARTICIPANT_ID = "diapason.agent";
const DIAPASON_TASK_TRACKER_TOOL = "diapason-taskTracker";
const TASK_TRACKER_TASK_COMMAND = "task-tracker";
const DIAPASON_MCP_PROVIDER_ID = "diapason.workspaceMcp";
const TRUST_PROMPT_STATE_KEY = "diapason.trustPromptShown";
const FULL_AUTO_WARNING_STATE_KEY = "diapason.fullAutoWarningShown";

const mcpDefinitionsChanged = new vscode.EventEmitter<void>();

let ideContextBridge: IdeContextFileBridge | undefined;

const CANONICAL_IDE_CONTEXT_ENV_VARIABLE = "DIAPASON_IDE_CONTEXT_FILE";
const LEGACY_IDE_CONTEXT_ENV_VARIABLE = "DIAPASON_VSCODE_CONTEXT_FILE";
const IDE_CONTEXT_SNAPSHOT_VERSION = 1;
const IDE_CONTEXT_HEADER = "## VS Code Context";
const MAX_IDE_CONTEXT_CHARS = 6000;
const MAX_FULL_DOCUMENT_CONTEXT_LINES = 400;
const ACTIVE_EDITOR_CONTEXT_WINDOW = 80;
const MAX_VISIBLE_EDITOR_CONTEXTS = 3;

type IdeContextProviderFamily = "vscode_compatible";

interface IdeContextSnapshot {
    readonly version: number;
    readonly provider_family: IdeContextProviderFamily;
    readonly editor_name?: string;
    readonly workspace_root?: string;
    readonly active_file?: IdeContextFileSnapshot;
    readonly visible_editors?: IdeContextFileSnapshot[];
}

interface IdeContextFileSnapshot {
    readonly path: string;
    readonly language_id?: string;
    readonly line_range?: IdeContextLineRange;
    readonly dirty: boolean;
    readonly truncated: boolean;
    readonly selection?: IdeContextSelectionSnapshot;
}

interface IdeContextLineRange {
    readonly start: number;
    readonly end: number;
}

interface IdeContextSelectionSnapshot {
    readonly range: IdeContextSelectionRange;
    readonly text?: string;
}

interface IdeContextSelectionRange {
    readonly start_line: number;
    readonly start_column: number;
    readonly end_line: number;
    readonly end_column: number;
}

interface IdeContextFileTarget {
    readonly directoryUri?: vscode.Uri;
    readonly snapshotUri: vscode.Uri;
    readonly legacyMarkdownUri: vscode.Uri;
}

export function activate(context: vscode.ExtensionContext) {
    activationContext = context;
    outputChannel = vscode.window.createOutputChannel("Diapason");
    context.subscriptions.push(outputChannel);

    // Initialize command registry with modular commands
    const commandRegistry = new CommandRegistry();

    // Register all modular commands
    commandRegistry.registerAll([
        new AskCommand(),
        new AskSelectionCommand(),
        new AnalyseCommand(),
        new TaskTrackerCommand(),
        new OpenConfigCommand(),
        new TrustWorkspaceCommand(),
        new RefreshCommand(),
        new FindFilesCommand(),
        new ListFilesCommand(),
        new SearchFilesCommand(),
    ]);

    // Add command registry to disposables
    context.subscriptions.push(commandRegistry);

    ensureStableApi(context);
    logExtensionHostContext(context);
    registerDiapasonLanguageFeatures(context);

    void initializeIdeContextBridge(context);

    statusBarItem = vscode.window.createStatusBarItem(
        vscode.StatusBarAlignment.Left,
        100
    );
    statusBarItem.name = "Diapason Quick Actions";
    statusBarItem.accessibilityInformation = {
        role: "button",
        label: "Open Diapason quick actions or installation guide",
    };
    context.subscriptions.push(statusBarItem);

    workspaceInsightsProvider = new WorkspaceInsightsTreeDataProvider(() =>
        createWorkspaceInsights(
            workspaceTrusted,
            cliAvailable,
            currentConfigSummary
        )
    );
    context.subscriptions.push(
        vscode.window.registerTreeDataProvider(
            "diapasonWorkspaceStatusView",
            workspaceInsightsProvider
        )
    );

    quickActionsProviderInstance = new QuickActionTreeDataProvider(() =>
        createQuickActions(cliAvailable, currentConfigSummary, workspaceTrusted)
    );
    context.subscriptions.push(
        vscode.window.registerTreeDataProvider(
            "diapasonQuickActionsView",
            quickActionsProviderInstance
        )
    );

    initializeContextKeys();

    void registerDiapasonConfigWatcher(context, handleConfigUpdate);

    updateWorkspaceTrustState(workspaceTrusted);
    void promptForWorkspaceTrustOnActivation(context);

    if (workspaceTrusted) {
        setStatusBarChecking(getConfiguredCommandPath());
    }

    context.subscriptions.push(
        vscode.workspace.onDidGrantWorkspaceTrust(async () => {
            updateWorkspaceTrustState(true);
            setStatusBarChecking(getConfiguredCommandPath());
            await refreshCliAvailability("manual");
        })
    );

    const workspaceFolderWatcher = vscode.workspace.onDidChangeWorkspaceFolders(
        () => {
            quickActionsProviderInstance?.refresh();
            workspaceInsightsProvider?.refresh();
            void refreshCliAvailability("manual");
        }
    );

    if (vscode.env.uiKind === vscode.UIKind.Web) {
        void vscode.window.showWarningMessage(
            "Diapason Companion is running in VS Code for the Web. Command execution features are disabled, but documentation and configuration helpers remain available."
        );
    }

    context.subscriptions.push(
        vscode.commands.registerCommand("diapason.openQuickActions", async () => {
            const actions = createQuickActions(
                cliAvailable,
                currentConfigSummary,
                workspaceTrusted
            );
            const pickItems: QuickActionItem[] = actions.map((action) => ({
                label: `${action.icon ? `$(${action.icon}) ` : ""}${
                    action.label
                }`,
                description: action.description,
                run: () =>
                    vscode.commands.executeCommand(
                        action.command,
                        ...(action.args ?? [])
                    ),
            }));

            const selection = await vscode.window.showQuickPick(pickItems, {
                placeHolder: "Choose a Diapason action to run",
            });

            if (selection) {
                await selection.run();
            }
        })
    );

    const verifyWorkspaceTrustCommand = vscode.commands.registerCommand(
        "diapason.verifyWorkspaceTrust",
        async () => {
            // Show current trust status
            if (vscode.workspace.isTrusted) {
                void vscode.window.showInformationMessage(
                    "Workspace is trusted. Diapason chat prompts will avoid the trust modal while tool executions still require approval."
                );
            } else {
                void vscode.window.showWarningMessage(
                    "Workspace is not trusted. Diapason requires a trusted workspace to execute commands and access tools."
                );
            }

            const outputChannel = getOutputChannel();
            outputChannel.appendLine(
                `[info] Workspace trust status: ${
                    vscode.workspace.isTrusted ? "trusted" : "not trusted"
                }`
            );

            // Optionally show workspace trust management
            if (!vscode.workspace.isTrusted) {
                const selection = await vscode.window.showInformationMessage(
                    "Trust is required for Diapason. Would you like to manage workspace trust settings?",
                    "Manage Trust Settings",
                    "Learn More"
                );

                if (selection === "Manage Trust Settings") {
                    await vscode.commands.executeCommand(
                        "workbench.action.manageTrust"
                    );
                } else if (selection === "Learn More") {
                    await vscode.env.openExternal(
                        vscode.Uri.parse(
                            "https://code.visualstudio.com/docs/editor/workspace-trust"
                        )
                    );
                }
            }
        }
    );

    const flushIdeContextCommand = vscode.commands.registerCommand(
        "diapason.flushIdeContextSnapshot",
        async () => {
            if (!ideContextBridge) {
                syncIdeContextEnvironmentVariable();
                return false;
            }
            await ideContextBridge.flush();
            syncIdeContextEnvironmentVariable();
            return true;
        }
    );

    const openDocumentation = vscode.commands.registerCommand(
        "diapason.openDocumentation",
        async () => {
            await vscode.env.openExternal(
                vscode.Uri.parse("https://github.com/leynos/vtcode#readme")
            );
        }
    );

    const openDeepWiki = vscode.commands.registerCommand(
        "diapason.openDeepWiki",
        async () => {
            await vscode.env.openExternal(
                vscode.Uri.parse("https://deepwiki.com/leynos/vtcode")
            );
        }
    );

    const openWalkthrough = vscode.commands.registerCommand(
        "diapason.openWalkthrough",
        async () => {
            await vscode.commands.executeCommand(
                "workbench.action.openWalkthrough",
                "diapason.walkthrough"
            );
        }
    );

    const openInstallGuide = vscode.commands.registerCommand(
        "diapason.openInstallGuide",
        async () => {
            await vscode.env.openExternal(
                vscode.Uri.parse(
                    "https://github.com/leynos/vtcode"
                )
            );
        }
    );

    const toggleHumanInTheLoopCommand = vscode.commands.registerCommand(
        "diapason.toggleHumanInTheLoop",
        async () => {
            if (
                !(await ensureWorkspaceTrustedForCommand(
                    "change Diapason human-in-the-loop settings"
                ))
            ) {
                return;
            }

            try {
                const configUri = await pickDiapasonConfigUri(
                    currentConfigSummary?.uri
                );
                if (!configUri) {
                    void vscode.window.showWarningMessage(
                        "No config.toml file was found in this workspace."
                    );
                    return;
                }

                const activeSummary =
                    currentConfigSummary &&
                    currentConfigSummary.uri?.toString() ===
                        configUri.toString()
                        ? currentConfigSummary
                        : await loadConfigSummaryFromUri(configUri);

                const newValue = activeSummary.humanInTheLoop === false;
                const updated = await setHumanInTheLoop(configUri, newValue);
                if (!updated) {
                    void vscode.window.showWarningMessage(
                        "Failed to update human_in_the_loop in config.toml."
                    );
                    return;
                }

                const relativePath = vscode.workspace.asRelativePath(
                    configUri,
                    false
                );
                const channel = getOutputChannel();
                channel.appendLine(
                    `[info] human_in_the_loop set to ${newValue} in ${relativePath}.`
                );
                void vscode.window.showInformationMessage(
                    `Human-in-the-loop safeguards are now ${
                        newValue ? "enabled" : "disabled"
                    } in config.toml.`
                );
            } catch (error) {
                handleCommandError("toggle human-in-the-loop mode", error);
            }
        }
    );

    const openToolsPolicyGuideCommand = vscode.commands.registerCommand(
        "diapason.openToolsPolicyGuide",
        async () => {
            try {
                await openToolsPolicyGuide();
            } catch (error) {
                handleCommandError("open tool policy guide", error);
            }
        }
    );

    const openToolsPolicyConfigCommand = vscode.commands.registerCommand(
        "diapason.openToolsPolicyConfig",
        async () => {
            try {
                const configUri = await pickDiapasonConfigUri(
                    currentConfigSummary?.uri
                );
                if (!configUri) {
                    void vscode.window.showWarningMessage(
                        "No config.toml file was found in this workspace."
                    );
                    return;
                }

                await revealToolsPolicySection(configUri);
            } catch (error) {
                handleCommandError("open tool policy configuration", error);
            }
        }
    );

    const configureMcpProvidersCommand = vscode.commands.registerCommand(
        "diapason.configureMcpProviders",
        async () => {
            if (
                !(await ensureWorkspaceTrustedForCommand(
                    "edit Diapason MCP provider settings"
                ))
            ) {
                return;
            }

            try {
                const configUri = await pickDiapasonConfigUri(
                    currentConfigSummary?.uri
                );
                if (!configUri) {
                    void vscode.window.showWarningMessage(
                        "No config.toml file was found in this workspace."
                    );
                    return;
                }

                const activeSummary =
                    currentConfigSummary &&
                    currentConfigSummary.uri?.toString() ===
                        configUri.toString()
                        ? currentConfigSummary
                        : await loadConfigSummaryFromUri(configUri);

                const providers = activeSummary.mcpProviders;
                const enabledCount = providers.filter(
                    (provider) => provider.enabled !== false
                ).length;

                const quickItems: Array<
                    vscode.QuickPickItem & {
                        action: "toggle" | "add" | "guide" | "open";
                        providerName?: string;
                    }
                > = providers.map((provider) => ({
                    label: `${
                        provider.enabled === false
                            ? "$(circle-slash)"
                            : "$(check)"
                    } ${provider.name}`,
                    description: provider.command ?? "No command configured",
                    detail:
                        provider.args && provider.args.length > 0
                            ? `Args: ${provider.args.join(" ")}`
                            : provider.enabled === false
                            ? "Provider disabled"
                            : undefined,
                    action: "toggle",
                    providerName: provider.name,
                }));

                quickItems.push(
                    {
                        label: "$(add) Add MCP provider",
                        description:
                            "Define a new Model Context Protocol provider entry.",
                        action: "add",
                    },
                    {
                        label: "$(gear) Open MCP configuration",
                        description: "Edit the MCP section in config.toml.",
                        action: "open",
                    },
                    {
                        label: "$(book) Open MCP integration guide",
                        description:
                            "Read the Diapason MCP configuration walkthrough.",
                        action: "guide",
                    }
                );

                const selection = await vscode.window.showQuickPick(
                    quickItems,
                    {
                        placeHolder:
                            providers.length > 0
                                ? `Manage ${providers.length} MCP provider${
                                      providers.length === 1 ? "" : "s"
                                  } (${enabledCount} enabled)`
                                : "No MCP providers defined. Add one to enable external tools.",
                    }
                );

                if (!selection) {
                    return;
                }

                switch (selection.action) {
                    case "toggle": {
                        if (!selection.providerName) {
                            return;
                        }

                        const provider = providers.find(
                            (candidate) =>
                                candidate.name === selection.providerName
                        );
                        if (!provider) {
                            void vscode.window.showWarningMessage(
                                `Provider “${selection.providerName}” is no longer available.`
                            );
                            return;
                        }

                        const newState = provider.enabled === false;
                        const result = await setMcpProviderEnabled(
                            configUri,
                            selection.providerName,
                            newState
                        );
                        if (result === "notfound") {
                            void vscode.window.showWarningMessage(
                                `Provider “${selection.providerName}” was not found in config.toml.`
                            );
                            return;
                        }

                        if (result === "updated") {
                            const channel = getOutputChannel();
                            const relativePath =
                                vscode.workspace.asRelativePath(
                                    configUri,
                                    false
                                );
                            channel.appendLine(
                                `[info] MCP provider "${selection.providerName}" enabled=${newState} in ${relativePath}.`
                            );
                            void vscode.window.showInformationMessage(
                                `MCP provider “${
                                    selection.providerName
                                }” is now ${newState ? "enabled" : "disabled"}.`
                            );
                        }
                        break;
                    }
                    case "add": {
                        const name = await vscode.window.showInputBox({
                            prompt: "Provider name",
                            ignoreFocusOut: true,
                        });

                        if (!name || !name.trim()) {
                            return;
                        }

                        if (
                            providers.some(
                                (provider) =>
                                    provider.name.toLowerCase() ===
                                    name.trim().toLowerCase()
                            )
                        ) {
                            void vscode.window.showWarningMessage(
                                `An MCP provider named “${name.trim()}” already exists.`
                            );
                            return;
                        }

                        const command = await vscode.window.showInputBox({
                            prompt: "Command used to launch the provider",
                            value: "uvx",
                            ignoreFocusOut: true,
                        });

                        if (!command || !command.trim()) {
                            return;
                        }

                        const argsInput = await vscode.window.showInputBox({
                            prompt: "Arguments (separate with spaces, leave blank for none)",
                            ignoreFocusOut: true,
                        });

                        const args = argsInput
                            ? argsInput
                                  .split(" ")
                                  .map((value) => value.trim())
                                  .filter((value) => value.length > 0)
                            : [];

                        const enableChoice = await vscode.window.showQuickPick(
                            ["Enable provider", "Keep disabled"],
                            {
                                placeHolder:
                                    "Should the provider start enabled?",
                            }
                        );

                        if (!enableChoice) {
                            return;
                        }

                        const appended = await appendMcpProvider(configUri, {
                            name: name.trim(),
                            command: command.trim(),
                            args,
                            enabled: enableChoice === "Enable provider",
                        });

                        if (appended) {
                            const channel = getOutputChannel();
                            const relativePath =
                                vscode.workspace.asRelativePath(
                                    configUri,
                                    false
                                );
                            channel.appendLine(
                                `[info] Added MCP provider "${name.trim()}" to ${relativePath}.`
                            );
                            void vscode.window.showInformationMessage(
                                `Added MCP provider “${name.trim()}” to config.toml.`
                            );
                        } else {
                            void vscode.window.showWarningMessage(
                                `Provider “${name.trim()}” already exists in config.toml.`
                            );
                        }
                        break;
                    }
                    case "guide": {
                        await openMcpGuide();
                        break;
                    }
                    case "open": {
                        await revealMcpSection(configUri);
                        break;
                    }
                }
            } catch (error) {
                handleCommandError("configure MCP providers", error);
            }
        }
    );

    const launchAgentTerminal = vscode.commands.registerCommand(
        "diapason.launchAgentTerminal",
        async () => {
            if (!(await ensureCliAvailableForCommand())) {
                return;
            }

            const cwd = getWorkspaceRoot();
            if (!cwd) {
                void vscode.window.showWarningMessage(
                    "Open a workspace folder before launching the Diapason agent terminal."
                );
                return;
            }

            const commandPath = getConfiguredCommandPath();
            const { terminal, created } = ensureAgentTerminal(commandPath, cwd);
            terminal.show(true);
            if (created) {
                const channel = getOutputChannel();
                channel.appendLine(
                    `[info] Launching Diapason agent terminal with "${commandPath} chat" in ${cwd}.`
                );
            }
        }
    );

    // Note: runAnalyze is registered via CommandRegistry (AnalyseCommand)
    // Note: runTaskTrackerTask is registered via CommandRegistry (TaskTrackerCommand)
    // Note: refreshQuickActions is registered via CommandRegistry (RefreshCommand)

    const taskProvider = vscode.tasks.registerTaskProvider("diapason", {
        provideTasks: provideDiapasonTasks,
        resolveTask: resolveDiapasonTask,
    });

    const configurationWatcher = vscode.workspace.onDidChangeConfiguration(
        (event) => {
            if (event.affectsConfiguration("diapason.commandPath")) {
                void refreshCliAvailability("configuration");
            }
        }
    );

    context.subscriptions.push(
        verifyWorkspaceTrustCommand,
        flushIdeContextCommand,
        openDocumentation,
        openDeepWiki,
        openWalkthrough,
        openInstallGuide,
        toggleHumanInTheLoopCommand,
        openToolsPolicyGuideCommand,
        openToolsPolicyConfigCommand,
        configureMcpProvidersCommand,
        launchAgentTerminal,
        taskProvider,
        configurationWatcher,
        workspaceFolderWatcher
    );

    registerDiapasonAiIntegrations(context);

    void refreshCliAvailability("activation");
}

async function initializeIdeContextBridge(
    context: vscode.ExtensionContext
): Promise<void> {
    const storageRoot = context.globalStorageUri ?? context.storageUri;
    if (!storageRoot) {
        return;
    }

    try {
        await vscode.workspace.fs.createDirectory(storageRoot);
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        getOutputChannel().appendLine(
            `[warn] Failed to prepare IDE context storage: ${message}`
        );
    }

    const jsonFileUri = vscode.Uri.joinPath(
        storageRoot,
        "diapason-ide-context.json"
    );
    const markdownFileUri = vscode.Uri.joinPath(
        storageRoot,
        "diapason-ide-context.md"
    );
    const bridge = new IdeContextFileBridge(jsonFileUri, markdownFileUri);
    ideContextBridge = bridge;
    syncIdeContextEnvironmentVariable();
    context.subscriptions.push(bridge);

    await bridge.flush();

    const scheduleRefresh = () => ideContextBridge?.scheduleRefresh();
    context.subscriptions.push(
        vscode.window.onDidChangeActiveTextEditor(() => scheduleRefresh()),
        vscode.window.onDidChangeVisibleTextEditors(() => scheduleRefresh()),
        vscode.window.onDidChangeTextEditorSelection(() => scheduleRefresh()),
        vscode.workspace.onDidChangeTextDocument((event) => {
            if (isDocumentVisible(event.document)) {
                scheduleRefresh();
            }
        }),
        vscode.workspace.onDidSaveTextDocument((document) => {
            if (isDocumentVisible(document)) {
                scheduleRefresh();
            }
        }),
        vscode.workspace.onDidCloseTextDocument(() => scheduleRefresh()),
        vscode.workspace.onDidChangeWorkspaceFolders(() => scheduleRefresh())
    );
}

export function deactivate() {
    ideContextBridge = undefined;
    syncIdeContextEnvironmentVariable();

    if (outputChannel) {
        outputChannel.dispose();
        outputChannel = undefined;
    }

    if (statusBarItem) {
        statusBarItem.dispose();
        statusBarItem = undefined;
    }

    if (agentTerminal) {
        agentTerminal.dispose();
        agentTerminal = undefined;
    }

    if (terminalCloseListener) {
        terminalCloseListener.dispose();
        terminalCloseListener = undefined;
    }

    quickActionsProviderInstance = undefined;
    cliAvailabilityCheck = undefined;
}

async function ensureCliAvailableForCommand(): Promise<boolean> {
    if (!(await ensureWorkspaceTrustedForCommand("run Diapason commands"))) {
        return false;
    }

    await refreshCliAvailability("manual");

    if (cliAvailable) {
        return true;
    }

    const commandPath = getConfiguredCommandPath();
    const selection = await vscode.window.showWarningMessage(
        `The Diapason CLI ("${commandPath}") is not available. Install the CLI or update the "diapason.commandPath" setting to run this command.`,
        "Open Installation Guide"
    );

    if (selection === "Open Installation Guide") {
        await vscode.commands.executeCommand("diapason.openInstallGuide");
    }

    return false;
}

function createQuickActions(
    cliAvailableState: boolean,
    summary: DiapasonConfigSummary | undefined,
    trusted: boolean
): QuickActionDescription[] {
    const actions: QuickActionDescription[] = [];

    if (!trusted) {
        actions.push(
            {
                label: "Trust this workspace for Diapason",
                description:
                    "Grant workspace trust to enable Diapason automation and CLI access.",
                command: "diapason.trustWorkspace",
                icon: "shield",
            },
            {
                label: "Verify workspace trust flow",
                description:
                    "Run the Diapason trust checklist so chat prompts stop requesting trust while tools stay gated.",
                command: "diapason.verifyWorkspaceTrust",
                icon: "shield-check",
            },
            {
                label: "Review Diapason CLI requirements",
                description:
                    "Learn how the Diapason CLI integrates once the workspace is trusted.",
                command: "diapason.openInstallGuide",
                icon: "tools",
            }
        );
    }

    if (trusted && cliAvailableState) {
        actions.push(
            {
                label: "Verify workspace trust flow",
                description:
                    "Confirm Diapason chat prompts avoid the trust modal while tool executions still require approval.",
                command: "diapason.verifyWorkspaceTrust",
                icon: "shield-check",
            },
            {
                label: "Refresh IDE context snapshot",
                description:
                    "Force a new IDE context snapshot so the agent sees your latest editor state.",
                command: "diapason.flushIdeContextSnapshot",
                icon: "history",
            },
            {
                label: "Ask the Diapason agent…",
                description:
                    "Send a one-off question and stream the answer in VS Code.",
                command: "diapason.askAgent",
                icon: "comment-discussion",
            },
            {
                label: "Ask about highlighted selection",
                description:
                    "Right-click or trigger Diapason to explain the selected text.",
                command: "diapason.askSelection",
                icon: "comment",
            },
            {
                label: "Run Diapason task tracker",
                description:
                    "Run the predefined VS Code task that drives the task_tracker tool.",
                command: "diapason.runTaskTrackerTask",
                icon: "checklist",
            },
            {
                label: "Launch interactive Diapason terminal",
                description:
                    "Open an integrated terminal session running diapason chat.",
                command: "diapason.launchAgentTerminal",
                icon: "terminal",
            },
            {
                label: "Analyse workspace with Diapason",
                description:
                    "Run diapason analyse and stream the report to the Diapason output channel.",
                command: "diapason.runAnalyze",
                icon: "pulse",
            }
        );
    } else if (trusted) {
        actions.push({
            label: "Review Diapason CLI installation",
            description:
                "Open the Diapason CLI installation instructions required for automation.",
            command: "diapason.openInstallGuide",
            icon: "tools",
        });
    }

    if (trusted && summary?.hasConfig) {
        if (summary.automationFullAutoEnabled === true) {
            actions.push({
                label: "Full-auto automation detected (blocked)",
                description:
                    "Open config.toml to disable [automation.full_auto]; VS Code will not run autonomous tasks.",
                command: "diapason.openConfig",
                icon: "shield-off",
            });
        }

        const hitlEnabled = summary.humanInTheLoop !== false;
        actions.push({
            label: hitlEnabled
                ? "Disable human-in-the-loop safeguards"
                : "Enable human-in-the-loop safeguards",
            description: hitlEnabled
                ? "Allow Diapason to automate tool execution without manual approval."
                : "Require confirmation before Diapason executes high-impact tools.",
            command: "diapason.toggleHumanInTheLoop",
            icon: "shield",
        });

        const providerCount = summary.mcpProviders.length;
        const enabledCount = summary.mcpProviders.filter(
            (provider) => provider.enabled !== false
        ).length;
        actions.push({
            label:
                providerCount > 0
                    ? "Manage MCP providers"
                    : "Configure MCP providers",
            description:
                providerCount > 0
                    ? `Adjust ${enabledCount}/${providerCount} enabled Model Context Protocol providers.`
                    : "Connect Diapason to external Model Context Protocol tools.",
            command: "diapason.configureMcpProviders",
            icon: "plug",
        });

        const toolPoliciesCount = summary.toolPoliciesCount ?? 0;
        actions.push({
            label: "Review tool policy configuration",
            description:
                toolPoliciesCount > 0
                    ? `Inspect ${toolPoliciesCount} explicit tool policy overrides.`
                    : "Define allow/prompt/deny rules for Diapason tools.",
            command: "diapason.openToolsPolicyConfig",
            icon: "law",
        });
    }

    const toolGuideDescription =
        summary?.hasConfig && trusted
            ? "Read documentation covering Diapason tool governance and HITL flows."
            : "Learn how Diapason enforces tool governance and human-in-the-loop safeguards.";
    actions.push({
        label: "Open Diapason tool policy guide",
        description: toolGuideDescription,
        command: "diapason.openToolsPolicyGuide",
        icon: "book",
    });

    const configDescription = summary?.uri
        ? `Open ${vscode.workspace.asRelativePath(
              summary.uri,
              false
          )} to adjust Diapason settings.`
        : "Jump directly to the workspace Diapason configuration file.";

    actions.push(
        {
            label: "Open config.toml",
            description: configDescription,
            command: "diapason.openConfig",
            icon: "gear",
        },
        {
            label: "View Diapason documentation",
            description: "Open the Diapason README in your browser.",
            command: "diapason.openDocumentation",
            icon: "book",
        },
        {
            label: "Review Diapason DeepWiki overview",
            description: "Open the DeepWiki page for Diapason capabilities.",
            command: "diapason.openDeepWiki",
            icon: "globe",
        },
        {
            label: "Explore the Diapason walkthrough",
            description:
                "Open the getting-started walkthrough to learn about Diapason features.",
            command: "diapason.openWalkthrough",
            icon: "rocket",
        }
    );

    return actions;
}

function createWorkspaceInsights(
    trusted: boolean,
    cliAvailableState: boolean,
    summary: DiapasonConfigSummary | undefined
): WorkspaceInsightDescription[] {
    const insights: WorkspaceInsightDescription[] = [];

    insights.push({
        label: trusted ? "Workspace trust granted" : "Workspace trust required",
        description: trusted
            ? "Diapason can run CLI automation in this workspace."
            : "Grant trust to enable Diapason CLI commands and automation features.",
        icon: trusted ? "shield" : "shield-off",
        command: trusted
            ? undefined
            : {
                  command: "diapason.trustWorkspace",
                  title: "Trust Workspace for Diapason",
              },
        tooltip: trusted
            ? "Workspace trust allows Diapason to spawn CLI processes."
            : "Security-sensitive features are disabled until this workspace is trusted.",
    });

    if (!trusted) {
        insights.push({
            label: "CLI access blocked",
            description:
                "Trust the workspace to allow Diapason to detect and launch the CLI.",
            icon: "circle-slash",
            command: {
                command: "diapason.openInstallGuide",
                title: "Review CLI Installation",
            },
        });
    } else {
        const commandPath = getConfiguredCommandPath();
        insights.push({
            label: cliAvailableState
                ? "Diapason CLI detected"
                : "Diapason CLI unavailable",
            description: cliAvailableState
                ? `Using ${commandPath}`
                : `Check ${commandPath} or adjust diapason.commandPath`,
            icon: cliAvailableState ? "check" : "warning",
            command: cliAvailableState
                ? {
                      command: "diapason.openQuickActions",
                      title: "Open Quick Actions",
                  }
                : {
                      command: "diapason.openInstallGuide",
                      title: "Open Installation Guide",
                  },
            tooltip: createStatusBarTooltip(
                commandPath,
                cliAvailableState,
                trusted
            ),
        });
    }

    if (summary?.hasConfig) {
        const configPath = summary.uri
            ? vscode.workspace.asRelativePath(summary.uri, false)
            : "config.toml";
        insights.push({
            label: "Diapason configuration detected",
            description: configPath,
            icon: "gear",
            command: {
                command: "diapason.openConfig",
                title: "Open config.toml",
            },
        });

        if (summary.agentProvider) {
            const provider = summary.agentProvider;
            const defaultModel = summary.agentDefaultModel;
            const providerLower = provider.toLowerCase();
            const modelLower = defaultModel?.toLowerCase() ?? "";
            const mismatch =
                (providerLower === "ollama" &&
                    (defaultModel?.includes(":") ?? false)) ||
                (providerLower !== "openrouter" &&
                    modelLower.startsWith("gpt-oss:"));

            insights.push({
                label: `Agent provider: ${provider}`,
                description: defaultModel
                    ? `Default model: ${defaultModel}`
                    : "No default model configured",
                icon: mismatch ? "alert" : "globe",
                command: mismatch
                    ? {
                          command: "diapason.openConfig",
                          title: "Review agent provider configuration",
                      }
                    : undefined,
                tooltip: mismatch
                    ? "Provider and default_model may require different credentials. Update config.toml to avoid CLI failures."
                    : undefined,
            });
        }

        const fullAutoEnabled = summary.automationFullAutoEnabled === true;
        const allowedTools = summary.automationFullAutoAllowedTools;
        const automationDescription = fullAutoEnabled
            ? allowedTools && allowedTools.length > 0
                ? `Allowed tools: ${allowedTools.join(
                      ", "
                  )}. VS Code blocks autonomous execution; disable automation.full_auto to avoid warnings.`
                : "automation.full_auto is enabled. VS Code blocks autonomous execution; disable the setting to silence this warning."
            : "automation.full_auto is disabled. Diapason prompts require explicit approval.";
        insights.push({
            label: fullAutoEnabled
                ? "Full-auto automation detected (blocked)"
                : "Full-auto automation disabled",
            description: automationDescription,
            icon: fullAutoEnabled ? "shield-off" : "shield",
            command: fullAutoEnabled
                ? {
                      command: "diapason.openConfig",
                      title: "Disable automation.full_auto",
                  }
                : undefined,
        });

        const hitlStatus =
            summary.humanInTheLoop === false
                ? "Disabled (manual approvals required)"
                : "Enabled";
        insights.push({
            label: "Human-in-the-loop safeguards",
            description: hitlStatus,
            icon: summary.humanInTheLoop === false ? "person" : "shield",
            command:
                trusted && summary.uri
                    ? {
                          command: "diapason.toggleHumanInTheLoop",
                          title: "Toggle human-in-the-loop safeguards",
                      }
                    : undefined,
        });

        const providerCount = summary.mcpProviders.length;
        const enabledCount = summary.mcpProviders.filter(
            (provider) => provider.enabled !== false
        ).length;
        insights.push({
            label: "MCP providers",
            description:
                providerCount > 0
                    ? `${enabledCount}/${providerCount} enabled`
                    : "No providers configured",
            icon: "plug",
            command:
                trusted && summary.uri
                    ? {
                          command: "diapason.configureMcpProviders",
                          title: "Configure MCP providers",
                      }
                    : undefined,
        });

        const toolPoliciesCount = summary.toolPoliciesCount ?? 0;
        const toolPolicyLabel = summary.toolDefaultPolicy
            ? `Default: ${summary.toolDefaultPolicy}`
            : "No default policy set";
        insights.push({
            label: "Tool policy coverage",
            description:
                toolPoliciesCount > 0
                    ? `${toolPoliciesCount} overrides · ${toolPolicyLabel}`
                    : `No overrides · ${toolPolicyLabel}`,
            icon: "law",
            command: {
                command: "diapason.openToolsPolicyConfig",
                title: "Review tool policy configuration",
            },
        });

        if (summary.parseError) {
            insights.push({
                label: "Configuration parsing error",
                description: summary.parseError,
                icon: "error",
                command: summary.uri
                    ? {
                          command: "diapason.openConfig",
                          title: "Open config.toml",
                      }
                    : undefined,
            });
        }
    } else {
        insights.push({
            label: "No config.toml detected",
            description:
                "Use Diapason: Open Configuration to create a workspace configuration.",
            icon: "file",
            command: {
                command: "diapason.openConfig",
                title: "Create config.toml",
            },
        });
    }

    return insights;
}

function handleConfigUpdate(summary: DiapasonConfigSummary) {
    currentConfigSummary = summary;

    void vscode.commands.executeCommand(
        "setContext",
        "diapason.configAvailable",
        summary.hasConfig
    );
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.hitlEnabled",
        summary.humanInTheLoop === true
    );
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.toolPoliciesConfigured",
        (summary.toolPoliciesCount ?? 0) > 0
    );
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.mcpConfigured",
        summary.mcpProviders.length > 0
    );
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.mcpEnabled",
        summary.mcpEnabled === true
    );
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.fullAutoEnabled",
        summary.automationFullAutoEnabled === true
    );

    const configUriString = summary.uri?.toString();
    if (summary.parseError && summary.parseError !== lastConfigParseError) {
        const channel = getOutputChannel();
        channel.appendLine(
            `[warn] Failed to parse config.toml: ${summary.parseError}`
        );
    } else if (
        !summary.parseError &&
        configUriString &&
        configUriString !== lastConfigUri
    ) {
        const channel = getOutputChannel();
        const label = summary.uri
            ? vscode.workspace.asRelativePath(summary.uri, false)
            : "config.toml";
        channel.appendLine(`[info] Using Diapason configuration from ${label}.`);
    }

    if (summary.hasConfig) {
        const enabled = summary.automationFullAutoEnabled === true;
        if (enabled && lastAutomationFullAutoEnabled !== true) {
            const channel = getOutputChannel();
            channel.appendLine(
                "[warn] Diapason extension: Full-auto permission review is blocked inside VS Code. Human-in-the-loop approval remains required."
            );
            void maybeWarnAboutFullAuto(summary);
        } else if (!enabled && lastAutomationFullAutoEnabled !== false) {
            const channel = getOutputChannel();
            channel.appendLine(
                "[info] automation.full_auto is disabled; Diapason will rely on CLI safeguards for prompt execution."
            );
        }
        lastAutomationFullAutoEnabled = enabled;
        maybeWarnAboutProviderModel(summary);
    } else {
        lastAutomationFullAutoEnabled = undefined;
        lastProviderModelWarningKey = undefined;
    }

    lastConfigUri = configUriString;
    lastConfigParseError = summary.parseError;

    updateStatusBarItem(getConfiguredCommandPath(), cliAvailable);
    quickActionsProviderInstance?.refresh();
    workspaceInsightsProvider?.refresh();
    mcpDefinitionsChanged.fire();
}

async function refreshCliAvailability(
    trigger: "activation" | "configuration" | "manual"
): Promise<void> {
    if (cliAvailabilityCheck) {
        await cliAvailabilityCheck;
        return;
    }

    const commandPath = getConfiguredCommandPath();

    if (!workspaceTrusted) {
        updateCliAvailabilityState(false, commandPath, "untrusted");
        return;
    }

    setStatusBarChecking(commandPath);

    cliAvailabilityCheck = (async () => {
        if (vscode.env.uiKind === vscode.UIKind.Web) {
            updateCliAvailabilityState(false, commandPath);
            return;
        }

        const available = await detectCliAvailability(commandPath);
        updateCliAvailabilityState(available, commandPath);

        if (!available && trigger === "activation" && workspaceTrusted) {
            await maybeShowMissingCliWarning(commandPath);
        }
    })();

    try {
        await cliAvailabilityCheck;
    } finally {
        cliAvailabilityCheck = undefined;
    }
}

async function detectCliAvailability(commandPath: string): Promise<boolean> {
    if (!commandPath) {
        return false;
    }

    const cwd = getWorkspaceRoot();
    const spawnOptions = cwd
        ? createSpawnOptions({ cwd })
        : createSpawnOptions();

    return new Promise((resolve) => {
        let resolved = false;

        const complete = (value: boolean) => {
            if (!resolved) {
                resolved = true;
                resolve(value);
            }
        };

        try {
            const child = spawn(commandPath, ["--version"], spawnOptions);

            const timer = setTimeout(() => {
                child.kill();
                complete(false);
            }, CLI_DETECTION_TIMEOUT_MS);

            child.on("error", () => {
                clearTimeout(timer);
                complete(false);
            });

            child.on("close", (code) => {
                clearTimeout(timer);
                complete(code === 0);
            });
        } catch {
            complete(false);
        }
    });
}

function updateCliAvailabilityState(
    available: boolean,
    commandPath: string,
    reason?: "untrusted"
) {
    const normalizedAvailable = available && workspaceTrusted;
    const previous = cliAvailable;
    cliAvailable = normalizedAvailable;

    void vscode.commands.executeCommand(
        "setContext",
        "diapason.cliAvailable",
        normalizedAvailable && workspaceTrusted
    );
    updateStatusBarItem(commandPath, normalizedAvailable);

    if (normalizedAvailable) {
        missingCliWarningShown = false;
    }

    if (previous !== normalizedAvailable) {
        const channel = getOutputChannel();
        if (!workspaceTrusted && reason === "untrusted") {
            channel.appendLine(
                "[info] Diapason CLI checks are paused until the workspace is trusted."
            );
        } else if (normalizedAvailable) {
            channel.appendLine(
                `[info] Detected Diapason CLI using "${commandPath}".`
            );
        } else {
            channel.appendLine(
                `[warn] Diapason CLI not found using "${commandPath}".`
            );
        }
    }

    quickActionsProviderInstance?.refresh();
    workspaceInsightsProvider?.refresh();
}

function setStatusBarChecking(commandPath: string) {
    if (!statusBarItem) {
        return;
    }

    if (!workspaceTrusted) {
        updateStatusBarItem(commandPath, false);
        return;
    }

    statusBarItem.text = "$(sync~spin) Diapason";
    statusBarItem.tooltip = `Checking availability of "${commandPath}"`;
    statusBarItem.command = undefined;
    statusBarItem.backgroundColor = undefined;
    statusBarItem.color = undefined;
    statusBarItem.show();
}

function updateStatusBarItem(commandPath: string, available: boolean) {
    if (!statusBarItem) {
        return;
    }

    if (!workspaceTrusted) {
        statusBarItem.text = "$(shield) Trust Diapason Workspace";
        statusBarItem.tooltip = createStatusBarTooltip(
            commandPath,
            available,
            false
        );
        statusBarItem.command = "workbench.action.manageTrust";
        statusBarItem.backgroundColor = new vscode.ThemeColor(
            "statusBarItem.prominentBackground"
        );
        statusBarItem.color = new vscode.ThemeColor(
            "statusBarItem.prominentForeground"
        );
        statusBarItem.show();
        return;
    }

    if (available) {
        const hitlEnabled = currentConfigSummary?.humanInTheLoop !== false;
        const suffix = currentConfigSummary?.hasConfig
            ? hitlEnabled
                ? " $(person)"
                : " $(run-all)"
            : "";
        statusBarItem.text = `$(chevron-right) Diapason${suffix}`; // Using chevron-right icon as requested

        statusBarItem.tooltip = createStatusBarTooltip(commandPath, true, true);
        statusBarItem.command = "diapason.openQuickActions";
        statusBarItem.backgroundColor = new vscode.ThemeColor(
            "diapason.statusBarBackground"
        );
        statusBarItem.color = new vscode.ThemeColor(
            "diapason.statusBarForeground"
        );
    } else {
        statusBarItem.text = "$(warning) Diapason CLI Missing";
        statusBarItem.tooltip = createStatusBarTooltip(
            commandPath,
            false,
            true
        );
        statusBarItem.command = "diapason.openInstallGuide";
        statusBarItem.backgroundColor = new vscode.ThemeColor(
            "statusBarItem.warningBackground"
        );
        statusBarItem.color = new vscode.ThemeColor(
            "statusBarItem.warningForeground"
        );
    }

    statusBarItem.show();
}

function createStatusBarTooltip(
    commandPath: string,
    available: boolean,
    trusted: boolean
): vscode.MarkdownString {
    const tooltip = new vscode.MarkdownString(undefined, true);
    tooltip.appendMarkdown("**Diapason Workspace**\n\n");
    tooltip.appendMarkdown(
        `• Workspace trust: ${trusted ? "Trusted" : "Restricted"}\n`
    );

    tooltip.appendMarkdown("\n**Diapason CLI**\n\n");
    tooltip.appendMarkdown(`• Path: \`${commandPath}\`\n`);
    const cliStatus = !trusted
        ? "Blocked by workspace trust"
        : available
        ? "Available"
        : "Missing";
    tooltip.appendMarkdown(`• Status: ${cliStatus}\n`);

    if (!trusted) {
        tooltip.appendMarkdown(
            "\nGrant workspace trust to enable Diapason CLI automation.\n"
        );
        tooltip.isTrusted = true;
        return tooltip;
    }

    if (currentConfigSummary?.hasConfig) {
        tooltip.appendMarkdown("\n**Configuration**\n");
        if (currentConfigSummary.uri) {
            const relative = vscode.workspace.asRelativePath(
                currentConfigSummary.uri,
                false
            );
            tooltip.appendMarkdown(`• File: \`${relative}\`\n`);
        }

        if (currentConfigSummary.humanInTheLoop !== undefined) {
            tooltip.appendMarkdown(
                `• Human-in-the-loop: ${
                    currentConfigSummary.humanInTheLoop ? "Enabled" : "Disabled"
                }\n`
            );
        }

        if (currentConfigSummary.toolDefaultPolicy) {
            tooltip.appendMarkdown(
                `• Default tool policy: \`${currentConfigSummary.toolDefaultPolicy}\`\n`
            );
        }

        const providerCount = currentConfigSummary.mcpProviders.length;
        const enabledCount = currentConfigSummary.mcpProviders.filter(
            (provider) => provider.enabled !== false
        ).length;
        if (providerCount > 0) {
            tooltip.appendMarkdown(
                `• MCP providers: ${enabledCount}/${providerCount} enabled\n`
            );
        } else {
            tooltip.appendMarkdown("• MCP providers: none configured\n");
        }
    } else {
        tooltip.appendMarkdown(
            "\nNo config.toml detected in this workspace.\n"
        );
    }

    tooltip.isTrusted = true;
    return tooltip;
}

async function maybeShowMissingCliWarning(commandPath: string): Promise<void> {
    if (missingCliWarningShown || vscode.env.uiKind === vscode.UIKind.Web) {
        return;
    }

    missingCliWarningShown = true;
    const selection = await vscode.window.showWarningMessage(
        `Diapason CLI was not found on PATH as "${commandPath}".`,
        "Open Installation Guide"
    );

    if (selection === "Open Installation Guide") {
        await vscode.commands.executeCommand("diapason.openInstallGuide");
    }
}

function updateWorkspaceTrustState(trusted: boolean): void {
    workspaceTrusted = trusted;
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.workspaceTrusted",
        trusted
    );
    void vscode.commands.executeCommand(
        "setContext",
        "diapason.workspaceTrustVerified",
        trusted
    );
    const commandPath = getConfiguredCommandPath();
    updateCliAvailabilityState(
        cliAvailable,
        commandPath,
        trusted ? undefined : "untrusted"
    );
    mcpDefinitionsChanged.fire();
    quickActionsProviderInstance?.refresh();
    workspaceInsightsProvider?.refresh();
}

function initializeContextKeys(): void {
    const contextDefaults: Array<[string, boolean]> = [
        ["diapason.workspaceTrusted", workspaceTrusted],
        ["diapason.cliAvailable", false],
        ["diapason.configAvailable", false],
        ["diapason.hitlEnabled", false],
        ["diapason.toolPoliciesConfigured", false],
        ["diapason.mcpConfigured", false],
        ["diapason.mcpEnabled", false],
        ["diapason.fullAutoEnabled", false],
        ["diapason.workspaceTrustVerified", workspaceTrusted],
    ];

    for (const [key, value] of contextDefaults) {
        void vscode.commands.executeCommand("setContext", key, value);
    }
}

type WorkspaceTrustApi = typeof vscode.workspace & {
    requestWorkspaceTrust?: (opts?: {
        message?: string;
        modal?: boolean;
        buttons?: ReadonlyArray<vscode.MessageItem>;
    }) => Thenable<boolean | undefined>;
};

async function requestWorkspaceTrust(action: string): Promise<boolean> {
    if (workspaceTrusted) {
        return true;
    }

    const trustApi = vscode.workspace as WorkspaceTrustApi;
    const requestFn = trustApi.requestWorkspaceTrust;
    if (typeof requestFn === "function") {
        try {
            const granted = await requestFn({
                message: `Diapason requires a trusted workspace to ${action}.`,
                modal: true,
            });
            if (granted) {
                updateWorkspaceTrustState(true);
                return true;
            }
        } catch (error) {
            const channel = getOutputChannel();
            const details =
                error instanceof Error ? error.message : String(error);
            channel.appendLine(
                `[warn] Workspace trust request failed: ${details}`
            );
        }
    }

    return false;
}

async function ensureWorkspaceTrustedForCommand(
    action: string
): Promise<boolean> {
    if (workspaceTrusted) {
        return true;
    }

    if (await requestWorkspaceTrust(action)) {
        return true;
    }

    const selection = await vscode.window.showWarningMessage(
        `Diapason requires a trusted workspace to ${action}.`,
        "Manage Workspace Trust"
    );

    if (selection === "Manage Workspace Trust") {
        await vscode.commands.executeCommand("workbench.action.manageTrust");
        const trustedNow = vscode.workspace.isTrusted;
        if (trustedNow) {
            updateWorkspaceTrustState(trustedNow);
            return true;
        }
    }

    return false;
}

async function promptForWorkspaceTrustOnActivation(
    context: vscode.ExtensionContext
): Promise<void> {
    if (workspaceTrusted || vscode.env.uiKind === vscode.UIKind.Web) {
        return;
    }

    const workspaceId =
        vscode.workspace.workspaceFolders?.[0]?.uri.toString() ?? "global";
    const storageKey = `${TRUST_PROMPT_STATE_KEY}:${workspaceId}`;
    const alreadyPrompted = context.globalState.get<boolean>(storageKey);
    if (alreadyPrompted) {
        return;
    }

    await context.globalState.update(storageKey, true);
    try {
        const granted = await requestWorkspaceTrust(
            "allow Diapason to process prompts with human-in-the-loop safeguards"
        );
        if (!granted && !workspaceTrusted) {
            const selection = await vscode.window.showInformationMessage(
                "Diapason requires workspace trust to process prompts. Open workspace trust settings?",
                "Manage Workspace Trust"
            );
            if (selection === "Manage Workspace Trust") {
                await vscode.commands.executeCommand(
                    "workbench.action.manageTrust"
                );
                if (vscode.workspace.isTrusted) {
                    updateWorkspaceTrustState(true);
                }
            }
        }
    } catch (error) {
        const channel = getOutputChannel();
        const details = error instanceof Error ? error.message : String(error);
        channel.appendLine(
            `[warn] Automatic workspace trust prompt failed: ${details}`
        );
    }
}

function getWorkspaceStorageKey(base: string): string {
    const workspaceId =
        vscode.workspace.workspaceFolders?.[0]?.uri.toString() ?? "global";
    return `${base}:${workspaceId}`;
}

async function maybeWarnAboutFullAuto(
    summary: DiapasonConfigSummary
): Promise<void> {
    if (!activationContext || summary.automationFullAutoEnabled !== true) {
        return;
    }

    const storageKey = getWorkspaceStorageKey(FULL_AUTO_WARNING_STATE_KEY);
    if (activationContext.globalState.get<boolean>(storageKey)) {
        return;
    }

    await activationContext.globalState.update(storageKey, true);
    const selection = await vscode.window.showWarningMessage(
        "Diapason full-auto permission review is blocked in VS Code. The extension will continue to require manual oversight before executing tools.",
        "Open config.toml",
        "Dismiss"
    );

    if (selection === "Open config.toml") {
        await vscode.commands.executeCommand("diapason.openConfig");
    }
}

function maybeWarnAboutProviderModel(summary: DiapasonConfigSummary): void {
    const provider = summary.agentProvider?.trim();
    const defaultModel = summary.agentDefaultModel?.trim();

    if (!provider || !defaultModel) {
        lastProviderModelWarningKey = undefined;
        return;
    }

    const key = `${provider}|${defaultModel}`;
    const providerLower = provider.toLowerCase();
    const modelLower = defaultModel.toLowerCase();

    let mismatch = false;
    if (providerLower === "ollama") {
        mismatch = defaultModel.includes(":");
    } else if (providerLower === "openrouter") {
        mismatch = false;
    } else if (modelLower.startsWith("gpt-oss:")) {
        mismatch = true;
    }

    if (!mismatch) {
        lastProviderModelWarningKey = undefined;
        return;
    }

    if (lastProviderModelWarningKey === key) {
        return;
    }

    lastProviderModelWarningKey = key;
    const channel = getOutputChannel();
    channel.appendLine(
        `[warn] Diapason config mismatch: provider "${provider}" may not support default_model "${defaultModel}". Update config.toml or configure the required API credentials to avoid CLI failures.`
    );
}

function getConfiguredCommandPath(): string {
    return (
        vscode.workspace
            .getConfiguration("diapason")
            .get<string>("commandPath", "diapason")
            .trim() || "diapason"
    );
}

interface RunDiapasonCommandOptions {
    readonly title?: string;
    readonly revealOutput?: boolean;
    readonly showProgress?: boolean;
    readonly onStdout?: (text: string) => void;
    readonly onStderr?: (text: string) => void;
    readonly cancellationToken?: vscode.CancellationToken;
}

interface TaskTrackerToolInput {
    readonly summary?: string;
    readonly steps?: string[];
}

async function runDiapasonCommand(
    args: string[],
    options: RunDiapasonCommandOptions = {}
): Promise<void> {
    if (vscode.env.uiKind === vscode.UIKind.Web) {
        throw new Error(
            "Diapason commands that spawn the CLI are not available in the web extension host."
        );
    }

    if (!workspaceTrusted) {
        throw new Error(
            "Trust this workspace to run Diapason CLI commands from VS Code."
        );
    }

    const commandPath = getConfiguredCommandPath();
    const cwd = getWorkspaceRoot();
    if (!cwd) {
        throw new Error(
            "Open a workspace folder before running Diapason commands."
        );
    }

    if (ideContextBridge) {
        await ideContextBridge.flush();
    }

    if (options.cancellationToken?.isCancellationRequested) {
        throw new vscode.CancellationError();
    }

    const channel = getOutputChannel();
    const configArgs = getConfigArguments();
    const normalizedArgs = args.map((arg) => String(arg));
    const finalArgs = [...configArgs, ...normalizedArgs];
    const displayArgs = formatArgsForLogging(finalArgs);
    const revealOutput = options.revealOutput ?? true;

    if (revealOutput) {
        channel.show(true);
    }
    channel.appendLine(`$ ${commandPath} ${displayArgs}`);

    const runCommand = async () =>
        new Promise<void>((resolve, reject) => {
            const child = spawn(
                commandPath,
                finalArgs,
                createSpawnOptions({ cwd })
            );

            let cancellationRegistration: vscode.Disposable | undefined;
            let cancelled = false;
            if (options.cancellationToken) {
                cancellationRegistration =
                    options.cancellationToken.onCancellationRequested(() => {
                        cancelled = true;
                        if (!child.killed) {
                            child.kill();
                        }
                    });
            }

            child.stdout.on("data", (data: Buffer) => {
                const text = data.toString();
                channel.append(text);
                options.onStdout?.(text);
            });

            child.stderr.on("data", (data: Buffer) => {
                const text = data.toString();
                channel.append(text);
                options.onStderr?.(text);
            });

            child.on("error", (error: Error) => {
                cancellationRegistration?.dispose();
                reject(error);
            });

            child.on("close", (code) => {
                cancellationRegistration?.dispose();
                if (cancelled) {
                    reject(new vscode.CancellationError());
                    return;
                }

                if (code === 0) {
                    resolve();
                } else {
                    reject(
                        new Error(
                            `Diapason exited with code ${code ?? "unknown"}`
                        )
                    );
                }
            });
        });

    if (options.showProgress === false) {
        await runCommand();
        return;
    }

    await vscode.window.withProgress(
        {
            location: vscode.ProgressLocation.Notification,
            title: options.title ?? "Running Diapason…",
        },
        runCommand
    );
}

function getConfigArguments(): string[] {
    const uri = currentConfigSummary?.uri;
    if (!uri) {
        return [];
    }

    return ["--config", uri.fsPath];
}

interface DiapasonTaskDefinition extends vscode.TaskDefinition {
    type: "diapason";
    command: "task-tracker";
    summary?: string;
    steps?: string[];
    label?: string;
}

async function provideDiapasonTasks(): Promise<vscode.Task[]> {
    if (vscode.env.uiKind === vscode.UIKind.Web) {
        return [];
    }

    if (!workspaceTrusted) {
        return [];
    }

    const folder = getPrimaryWorkspaceFolder();
    if (!folder) {
        return [];
    }

    if (ideContextBridge) {
        await ideContextBridge.flush();
    }

    const definition: DiapasonTaskDefinition = {
        type: "diapason",
        command: TASK_TRACKER_TASK_COMMAND,
        label: "Run task tracker with Diapason",
    };

    return [createTaskTrackerTask(folder, definition)];
}

function resolveDiapasonTask(task: vscode.Task): vscode.Task | undefined {
    const definition = task.definition as DiapasonTaskDefinition;
    if (definition.command !== TASK_TRACKER_TASK_COMMAND) {
        return undefined;
    }

    const scope = task.scope;
    let folder: vscode.WorkspaceFolder | undefined;
    if (scope && typeof scope !== "number") {
        folder = scope as vscode.WorkspaceFolder;
    } else {
        folder = getPrimaryWorkspaceFolder();
    }

    if (!folder) {
        return undefined;
    }

    ideContextBridge?.scheduleRefresh();

    return createTaskTrackerTask(folder, definition);
}

function createTaskTrackerTask(
    folder: vscode.WorkspaceFolder,
    definition: DiapasonTaskDefinition
): vscode.Task {
    const resolvedDefinition: DiapasonTaskDefinition = {
        type: "diapason",
        command: TASK_TRACKER_TASK_COMMAND,
        summary: definition.summary,
        steps: definition.steps,
        label: definition.label,
    };

    const label = definition.label ?? "Run task tracker with Diapason";
    const prompt = buildTaskTrackerPrompt(resolvedDefinition);
    const args = [...getConfigArguments(), "exec", prompt];

    const execution = new vscode.ProcessExecution(
        getConfiguredCommandPath(),
        args,
        {
            cwd: folder.uri.fsPath,
            env: getDiapasonEnvironment(),
        }
    );

    const task = new vscode.Task(
        resolvedDefinition,
        folder,
        label,
        "Diapason",
        execution
    );
    task.detail =
        "Runs `diapason exec` to synchronize the workspace TODO plan via the task_tracker tool.";
    task.presentationOptions = {
        reveal: vscode.TaskRevealKind.Always,
        echo: true,
        focus: false,
        panel: vscode.TaskPanelKind.Shared,
    };

    return task;
}

function buildTaskTrackerPrompt(definition: DiapasonTaskDefinition): string {
    const summary =
        definition.summary?.trim() ??
        "Refresh the current TODO plan using the Diapason task_tracker tool.";
    const steps = (definition.steps ?? [])
        .map((step) => step.trim())
        .filter((step) => step.length > 0);

    const lines = [
        summary,
        "Use the task_tracker tool to synchronize tasks and mark step status accurately.",
    ];

    if (steps.length > 0) {
        lines.push("", "Plan inputs:");
        steps.forEach((step, index) => {
            lines.push(`${index + 1}. ${step}`);
        });
    }

    return lines.join("\n");
}

function registerDiapasonAiIntegrations(context: vscode.ExtensionContext): void {
    context.subscriptions.push(mcpDefinitionsChanged);
    const contributedLanguageModelTools = Array.isArray(
        context.extension.packageJSON?.contributes?.languageModelTools
    )
        ? context.extension.packageJSON.contributes.languageModelTools
        : [];
    const hasTaskTrackerToolContribution = contributedLanguageModelTools.some(
        (tool) => tool?.name === DIAPASON_TASK_TRACKER_TOOL
    );
    const contributedChatParticipants = Array.isArray(
        context.extension.packageJSON?.contributes?.chatParticipants
    )
        ? context.extension.packageJSON.contributes.chatParticipants
        : [];
    const hasChatParticipantContribution = contributedChatParticipants.some(
        (participant) => participant?.id === DIAPASON_CHAT_PARTICIPANT_ID
    );

    if (
        hasTaskTrackerToolContribution &&
        "lm" in vscode &&
        typeof vscode.lm?.registerTool === "function"
    ) {
        const toolDisposable = vscode.lm.registerTool<TaskTrackerToolInput>(
            DIAPASON_TASK_TRACKER_TOOL,
            {
                prepareInvocation: async (options) => {
                    const summary = options.input.summary?.trim();
                    const invocationMessage = summary
                        ? `Running Diapason task tracker: ${summary}`
                        : "Running Diapason task tracker with diapason exec.";
                    return { invocationMessage };
                },
                invoke: async (options, token) => {
                    if (vscode.env.uiKind === vscode.UIKind.Web) {
                        throw new Error(
                            "Diapason CLI commands are not available in VS Code for the Web."
                        );
                    }

                    if (!workspaceTrusted) {
                        throw new Error(
                            "Trust this workspace to allow Diapason task tracker updates."
                        );
                    }

                    await refreshCliAvailability("manual");
                    if (!cliAvailable) {
                        const commandPath = getConfiguredCommandPath();
                        throw new Error(
                            `The Diapason CLI ("${commandPath}") is not available. Install the CLI or update the diapason.commandPath setting.`
                        );
                    }

                    const input = options.input ?? {};
                    const summary =
                        typeof input.summary === "string"
                            ? input.summary.trim()
                            : undefined;
                    const steps = Array.isArray(input.steps)
                        ? input.steps
                              .map((value) => String(value).trim())
                              .filter((value) => value.length > 0)
                        : undefined;

                    const definition: DiapasonTaskDefinition = {
                        type: "diapason",
                        command: TASK_TRACKER_TASK_COMMAND,
                        summary,
                        steps,
                    };
                    const prompt = buildTaskTrackerPrompt(definition);
                    const outputChunks: string[] = [];

                    try {
                        await runDiapasonCommand(["exec", prompt], {
                            title: "Running Diapason task tracker…",
                            revealOutput: false,
                            showProgress: false,
                            cancellationToken: token,
                            onStdout: (text) => outputChunks.push(text),
                            onStderr: (text) => outputChunks.push(text),
                        });
                    } catch (error) {
                        if (error instanceof vscode.CancellationError) {
                            throw error;
                        }

                        throw error;
                    }

                    const combined = outputChunks.join("");
                    const normalized = combined.replace(/\r\n/g, "\n").trim();
                    const content =
                        normalized.length > 0
                            ? `\`\`\`\n${normalized}\n\`\`\``
                            : "Diapason completed the task_tracker request but did not emit any output.";

                    return {
                        content: [new vscode.LanguageModelTextPart(content)],
                    };
                },
            }
        );
        context.subscriptions.push(toolDisposable);
    }

    if (
        "lm" in vscode &&
        typeof vscode.lm?.registerMcpServerDefinitionProvider === "function"
    ) {
        const mcpProvider: vscode.McpServerDefinitionProvider<vscode.McpStdioServerDefinition> =
            {
                onDidChangeMcpServerDefinitions: mcpDefinitionsChanged.event,
                provideMcpServerDefinitions: async () => {
                    if (!workspaceTrusted) {
                        return [];
                    }

                    const providers = currentConfigSummary?.mcpProviders ?? [];
                    return providers
                        .filter((provider) => provider.command)
                        .map(
                            (provider) =>
                                new vscode.McpStdioServerDefinition(
                                    provider.name,
                                    provider.command ?? "",
                                    provider.args ?? []
                                )
                        );
                },
                resolveMcpServerDefinition: async (server) => {
                    if (!workspaceTrusted) {
                        throw new Error(
                            "Trust this workspace before starting Diapason MCP servers."
                        );
                    }

                    return server;
                },
            };

        const disposable = vscode.lm.registerMcpServerDefinitionProvider(
            DIAPASON_MCP_PROVIDER_ID,
            mcpProvider
        );
        context.subscriptions.push(disposable);
    }

    if (
        hasChatParticipantContribution &&
        "chat" in vscode &&
        typeof vscode.chat?.createChatParticipant === "function"
    ) {
        try {
            const participant = vscode.chat.createChatParticipant(
                DIAPASON_CHAT_PARTICIPANT_ID,
                async (request, _context, response, token) => {
                    const basePrompt = request.prompt.trim();
                    if (!basePrompt) {
                        response.markdown(
                            "Ask a question or describe a task for the Diapason agent to begin."
                        );
                        return;
                    }

                    if (vscode.env.uiKind === vscode.UIKind.Web) {
                        const message =
                            "The Diapason CLI is not available in VS Code for the Web. Open a desktop workspace to chat with the CLI-driven agent.";
                        response.markdown(message);
                        return {
                            errorDetails: { message },
                        };
                    }

                    if (!workspaceTrusted) {
                        const message =
                            "Trust this workspace to allow Diapason to run CLI commands from chat.";
                        response.markdown(message);
                        return {
                            errorDetails: { message },
                        };
                    }

                    await refreshCliAvailability("manual");
                    if (!cliAvailable) {
                        const commandPath = getConfiguredCommandPath();
                        const message = `The Diapason CLI (\`${commandPath}\`) is not available. Install it or update the \`diapason.commandPath\` setting.`;
                        response.markdown(message);
                        return {
                            errorDetails: { message },
                        };
                    }

                    const promptWithContext = await appendIdeContextToPrompt(
                        basePrompt,
                        {
                            includeActiveEditor: true,
                            chatRequest: request,
                            cancellationToken: token,
                        }
                    );

                    response.progress("Running `diapason ask`…");

                    const collected: string[] = [];
                    try {
                        await runDiapasonCommand(["ask", promptWithContext], {
                            title: "Asking Diapason…",
                            revealOutput: false,
                            showProgress: false,
                            cancellationToken: token,
                            onStdout: (text) => collected.push(text),
                            onStderr: (text) => collected.push(text),
                        });
                    } catch (error) {
                        if (error instanceof vscode.CancellationError) {
                            response.progress(
                                "Diapason chat request cancelled."
                            );
                            return;
                        }

                        const message =
                            error instanceof Error
                                ? error.message
                                : String(error);
                        response.markdown(
                            `Diapason encountered an error while running the CLI: ${message}`
                        );
                        return {
                            errorDetails: { message },
                        };
                    }

                    const combined = collected.join("");
                    const normalized = combined.replace(/\r\n/g, "\n").trim();
                    if (normalized.length > 0) {
                        response.markdown(`\`\`\`\n${normalized}\n\`\`\``);
                    } else {
                        response.markdown(
                            "Diapason completed the request but did not emit any output."
                        );
                    }

                    return {
                        metadata: {
                            command: "ask",
                        },
                    };
                }
            );
            participant.iconPath = new vscode.ThemeIcon("rocket");
            participant.followupProvider = {
                provideFollowups: async () => [
                    {
                        prompt: "Summarize the current TODO plan.",
                        label: "Summarize TODO plan",
                    },
                    {
                        prompt: "Review configured MCP providers and highlight anything that needs attention.",
                        label: "Audit MCP providers",
                    },
                    {
                        prompt: "Suggest the next high-priority tasks Diapason should tackle in this workspace.",
                        label: "Suggest next tasks",
                    },
                ],
            };
            context.subscriptions.push(participant);
        } catch (error) {
            const message =
                error instanceof Error ? error.message : String(error);
            getOutputChannel().appendLine(
                `[warn] Failed to register Diapason chat participant: ${message}`
            );
        }
    }
}

interface AppendIdeContextOptions {
    readonly includeActiveEditor?: boolean;
    readonly includeVisibleEditors?: boolean;
    readonly chatRequest?: vscode.ChatRequest;
    readonly cancellationToken?: vscode.CancellationToken;
}

async function appendIdeContextToPrompt(
    prompt: string,
    options: AppendIdeContextOptions = {}
): Promise<string> {
    const contextBlock = await buildIdeContextBlock(options);
    if (!contextBlock) {
        return prompt;
    }

    const trimmedPrompt = prompt.trimEnd();
    const basePrompt = trimmedPrompt.length > 0 ? trimmedPrompt : prompt;

    if (basePrompt.length === 0) {
        return contextBlock;
    }

    return `${basePrompt}\n\n${contextBlock}`;
}

async function buildIdeContextBlock(
    options: AppendIdeContextOptions = {}
): Promise<string | undefined> {
    const sections = await collectIdeContextSections(options);
    if (sections.length === 0) {
        return undefined;
    }

    return [IDE_CONTEXT_HEADER, ...sections].join("\n\n");
}

async function collectIdeContextSections(
    options: AppendIdeContextOptions = {}
): Promise<string[]> {
    const sections: string[] = [];
    const seenKeys = new Set<string>();
    const token = options.cancellationToken;

    if (token?.isCancellationRequested) {
        throw new vscode.CancellationError();
    }

    if (options.includeActiveEditor !== false) {
        const activeSection = await buildActiveEditorContextSection(
            seenKeys,
            token
        );
        if (activeSection) {
            sections.push(activeSection);
        }
    }

    if (options.includeVisibleEditors) {
        const visibleSections = await buildVisibleEditorContextSections(
            seenKeys,
            token
        );
        if (visibleSections.length > 0) {
            sections.push(...visibleSections);
        }
    }

    if (options.chatRequest) {
        const referenceSections = await buildReferenceContextSections(
            options.chatRequest,
            seenKeys,
            token
        );
        if (referenceSections.length > 0) {
            sections.push(...referenceSections);
        }
    }

    return sections;
}

async function buildActiveEditorContextSection(
    seen: Set<string>,
    token?: vscode.CancellationToken
): Promise<string | undefined> {
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
        return undefined;
    }

    if (token?.isCancellationRequested) {
        throw new vscode.CancellationError();
    }

    const document = editor.document;
    const preferredRange = computeActiveEditorRange(editor);
    const context = extractDocumentContext(document, preferredRange);
    if (!context) {
        return undefined;
    }

    const key = createContextKey(document.uri, context.range, "active-editor");
    if (!registerContextKey(seen, key)) {
        return undefined;
    }

    const label = getPathLabel(document.uri);
    const detailParts: string[] = [];
    if (context.range) {
        detailParts.push(`lines ${formatRangeLabel(context.range)}`);
    }
    if (document.isDirty) {
        detailParts.push("unsaved changes");
    }
    if (context.truncated) {
        detailParts.push("truncated");
    }

    const headingDetails =
        detailParts.length > 0 ? ` (${detailParts.join(" • ")})` : "";
    const heading = `### Active Editor: ${label}${headingDetails}`;
    const codeBlock = formatCodeBlock(document.languageId, context.text);
    const notes = context.truncated
        ? "_Context truncated to fit VS Code chat limits._"
        : undefined;

    return [heading, codeBlock, notes].filter(Boolean).join("\n\n");
}

async function buildVisibleEditorContextSections(
    seen: Set<string>,
    token?: vscode.CancellationToken
): Promise<string[]> {
    const sections: string[] = [];
    const activeEditor = vscode.window.activeTextEditor;
    const activeUri = activeEditor?.document.uri.toString();

    for (const editor of vscode.window.visibleTextEditors) {
        if (sections.length >= MAX_VISIBLE_EDITOR_CONTEXTS) {
            break;
        }

        if (token?.isCancellationRequested) {
            throw new vscode.CancellationError();
        }

        const document = editor.document;
        if (document.uri.toString() === activeUri) {
            continue;
        }

        const context = extractDocumentContext(document, undefined);
        if (!context) {
            continue;
        }

        const key = createContextKey(
            document.uri,
            context.range,
            "visible-editor"
        );
        if (!registerContextKey(seen, key)) {
            continue;
        }

        const label = getPathLabel(document.uri);
        const detailParts: string[] = [];
        if (context.range) {
            detailParts.push(`lines ${formatRangeLabel(context.range)}`);
        }
        if (document.isDirty) {
            detailParts.push("unsaved changes");
        }
        if (context.truncated) {
            detailParts.push("truncated");
        }

        const headingDetails =
            detailParts.length > 0 ? ` (${detailParts.join(" • ")})` : "";
        const heading = `### Editor: ${label}${headingDetails}`;
        const codeBlock = formatCodeBlock(document.languageId, context.text);
        const notes = context.truncated
            ? "_Context truncated to fit VS Code chat limits._"
            : undefined;

        sections.push([heading, codeBlock, notes].filter(Boolean).join("\n\n"));
    }

    return sections;
}

async function buildReferenceContextSections(
    request: vscode.ChatRequest,
    seen: Set<string>,
    token?: vscode.CancellationToken
): Promise<string[]> {
    const sections: string[] = [];

    for (const reference of request.references ?? []) {
        if (token?.isCancellationRequested) {
            throw new vscode.CancellationError();
        }

        const section = await buildReferenceContextSection(
            reference,
            seen,
            token
        );
        if (section) {
            sections.push(section);
        }
    }

    return sections;
}

async function buildReferenceContextSection(
    reference: vscode.ChatPromptReference,
    seen: Set<string>,
    token?: vscode.CancellationToken
): Promise<string | undefined> {
    const value = reference.value;

    if (typeof value === "string") {
        const trimmed = value.trim();
        if (!trimmed) {
            return undefined;
        }

        const key = createContextKey(undefined, undefined, `string:${trimmed}`);
        if (!registerContextKey(seen, key)) {
            return undefined;
        }

        const description = reference.modelDescription?.trim();
        const headingLabel =
            description && description.length > 0
                ? description
                : `Reference ${reference.id}`;
        const heading = `### Reference: ${headingLabel}`;
        const block = formatCodeBlock("text", trimmed);
        return `${heading}\n\n${block}`;
    }

    if (value instanceof vscode.Location) {
        const document = await vscode.workspace.openTextDocument(value.uri);
        if (token?.isCancellationRequested) {
            throw new vscode.CancellationError();
        }

        const context = extractDocumentContext(document, value.range);
        if (!context) {
            return undefined;
        }

        const key = createContextKey(value.uri, context.range, reference.id);
        if (!registerContextKey(seen, key)) {
            return undefined;
        }

        const label = getPathLabel(value.uri);
        const description = reference.modelDescription?.trim();
        const headingLabel =
            description && description.length > 0 ? description : label;
        const details: string[] = [`lines ${formatRangeLabel(context.range)}`];
        if (context.truncated) {
            details.push("truncated");
        }
        const detailText =
            details.length > 0 ? ` (${details.join(" • ")})` : "";
        const heading = `### Reference: ${headingLabel}${detailText}`;
        const block = formatCodeBlock(document.languageId, context.text);
        const notes = context.truncated
            ? "_Context truncated to fit VS Code chat limits._"
            : undefined;
        return [heading, block, notes].filter(Boolean).join("\n\n");
    }

    if (value instanceof vscode.Uri) {
        const document = await vscode.workspace.openTextDocument(value);
        if (token?.isCancellationRequested) {
            throw new vscode.CancellationError();
        }

        const context = extractDocumentContext(document, undefined);
        if (!context) {
            return undefined;
        }

        const key = createContextKey(value, context.range, reference.id);
        if (!registerContextKey(seen, key)) {
            return undefined;
        }

        const label = getPathLabel(value);
        const description = reference.modelDescription?.trim();
        const headingLabel =
            description && description.length > 0 ? description : label;
        const details: string[] = [];
        if (context.range) {
            details.push(`lines ${formatRangeLabel(context.range)}`);
        }
        if (context.truncated) {
            details.push("truncated");
        }
        const detailText =
            details.length > 0 ? ` (${details.join(" • ")})` : "";
        const heading = `### Reference: ${headingLabel}${detailText}`;
        const block = formatCodeBlock(document.languageId, context.text);
        const notes = context.truncated
            ? "_Context truncated to fit VS Code chat limits._"
            : undefined;
        return [heading, block, notes].filter(Boolean).join("\n\n");
    }

    return undefined;
}

function computeActiveEditorRange(
    editor: vscode.TextEditor
): vscode.Range | undefined {
    const document = editor.document;
    if (!editor.selection.isEmpty) {
        return editor.selection;
    }

    const visibleRanges = editor.visibleRanges.filter(
        (range) => !range.isEmpty
    );
    if (visibleRanges.length > 0) {
        const first = visibleRanges[0];
        const last = visibleRanges[visibleRanges.length - 1];
        return new vscode.Range(first.start, last.end);
    }

    if (document.lineCount === 0) {
        return undefined;
    }

    if (document.lineCount <= MAX_FULL_DOCUMENT_CONTEXT_LINES) {
        const lastLineIndex = Math.max(0, document.lineCount - 1);
        const endPosition = document.lineAt(lastLineIndex).range.end;
        return new vscode.Range(new vscode.Position(0, 0), endPosition);
    }

    const activeLine = editor.selection.active.line;
    const halfWindow = Math.max(
        1,
        Math.floor(ACTIVE_EDITOR_CONTEXT_WINDOW / 2)
    );
    const startLine = Math.max(0, activeLine - halfWindow);
    const endLine = Math.min(document.lineCount - 1, activeLine + halfWindow);
    const endPosition = document.lineAt(endLine).range.end;
    return new vscode.Range(new vscode.Position(startLine, 0), endPosition);
}

interface DocumentContext {
    readonly text: string;
    readonly range: vscode.Range;
    readonly truncated: boolean;
}

interface IdeContextFileMetadata {
    readonly line_range?: IdeContextLineRange;
    readonly truncated: boolean;
}

interface IdeContextSelectionResult {
    readonly selection?: IdeContextSelectionSnapshot;
    readonly truncated: boolean;
}

function buildIdeContextSnapshot(): IdeContextSnapshot | undefined {
    const activeFile = buildActiveEditorSnapshot();
    const visibleEditors = buildVisibleEditorSnapshots(activeFile?.path);

    if (!activeFile && visibleEditors.length === 0) {
        return undefined;
    }

    const workspaceRoot = getPrimaryWorkspaceFolder()?.uri.fsPath;
    return {
        version: IDE_CONTEXT_SNAPSHOT_VERSION,
        provider_family: "vscode_compatible",
        editor_name: currentIdeDisplayName(),
        workspace_root: workspaceRoot,
        active_file: activeFile,
        visible_editors:
            visibleEditors.length > 0 ? visibleEditors : undefined,
    };
}

function currentIdeDisplayName(): string {
    const appName = vscode.env.appName?.trim().toLowerCase() ?? "";
    if (appName.includes("kiro")) {
        return "Kiro";
    }
    if (appName.includes("cursor")) {
        return "Cursor";
    }
    if (appName.includes("windsurf")) {
        return "Windsurf";
    }
    if (appName.includes("vscodium")) {
        return "VSCodium";
    }
    if (appName.includes("insider")) {
        return "VS Code Insiders";
    }
    return "VS Code";
}

function buildActiveEditorSnapshot():
    IdeContextFileSnapshot | undefined {
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
        return undefined;
    }

    return buildEditorFileSnapshot(editor, computeActiveEditorRange(editor));
}

function buildVisibleEditorSnapshots(
    activePath: string | undefined
): IdeContextFileSnapshot[] {
    const snapshots: IdeContextFileSnapshot[] = [];
    const seenPaths = new Set<string>();
    if (activePath) {
        seenPaths.add(activePath);
    }

    for (const editor of vscode.window.visibleTextEditors) {
        if (snapshots.length >= MAX_VISIBLE_EDITOR_CONTEXTS) {
            break;
        }

        const snapshot = buildEditorFileSnapshot(editor, undefined);
        if (!snapshot || seenPaths.has(snapshot.path)) {
            continue;
        }

        seenPaths.add(snapshot.path);
        snapshots.push(snapshot);
    }

    return snapshots;
}

function buildEditorFileSnapshot(
    editor: vscode.TextEditor,
    preferredRange: vscode.Range | undefined
): IdeContextFileSnapshot | undefined {
    const document = editor.document;
    const metadata = buildIdeContextFileMetadata(document, preferredRange);
    if (!metadata.line_range) {
        return undefined;
    }
    const selection = buildSelectionSnapshot(editor);

    const languageId = document.languageId.trim();
    return {
        path: getSnapshotPath(document.uri),
        language_id: languageId.length > 0 ? languageId : undefined,
        line_range: metadata.line_range,
        dirty: document.isDirty,
        truncated: metadata.truncated || selection.truncated,
        selection: selection.selection,
    };
}

function buildSelectionSnapshot(
    editor: vscode.TextEditor
): IdeContextSelectionResult {
    if (editor.selection.isEmpty) {
        return { truncated: false };
    }

    const limited = truncateForPrompt(
        normalizeForPrompt(editor.document.getText(editor.selection)),
        MAX_IDE_CONTEXT_CHARS
    );
    const text =
        limited.text.trim().length > 0 ? limited.text : undefined;

    return {
        selection: {
            range: {
                start_line: editor.selection.start.line + 1,
                start_column: editor.selection.start.character + 1,
                end_line: editor.selection.end.line + 1,
                end_column: editor.selection.end.character + 1,
            },
            text,
        },
        truncated: limited.truncated,
    };
}

function rangeToLineRange(range: vscode.Range): IdeContextLineRange {
    return {
        start: range.start.line + 1,
        end: range.end.line + 1,
    };
}

function getSnapshotPath(uri: vscode.Uri): string {
    if (uri.scheme === "file") {
        return uri.fsPath;
    }

    return uri.toString(true);
}

function buildIdeContextFileMetadata(
    document: vscode.TextDocument,
    preferredRange: vscode.Range | undefined
): IdeContextFileMetadata {
    if (document.lineCount === 0) {
        return { truncated: false };
    }

    if (preferredRange) {
        return {
            line_range: rangeToLineRange(preferredRange),
            truncated:
                document.lineCount >
                preferredRange.end.line - preferredRange.start.line + 1,
        };
    }

    const endLine = Math.min(document.lineCount, MAX_FULL_DOCUMENT_CONTEXT_LINES);
    return {
        line_range: { start: 1, end: endLine },
        truncated: document.lineCount > MAX_FULL_DOCUMENT_CONTEXT_LINES,
    };
}

function extractDocumentContext(
    document: vscode.TextDocument,
    range: vscode.Range | undefined
): DocumentContext | undefined {
    if (document.lineCount === 0) {
        return undefined;
    }

    let targetRange = range;
    let truncated = false;

    if (!targetRange) {
        const totalLines = document.lineCount;
        const endLineIndex =
            Math.min(totalLines, MAX_FULL_DOCUMENT_CONTEXT_LINES) - 1;
        const endPosition = document.lineAt(Math.max(0, endLineIndex)).range
            .end;
        targetRange = new vscode.Range(new vscode.Position(0, 0), endPosition);
        if (totalLines > MAX_FULL_DOCUMENT_CONTEXT_LINES) {
            truncated = true;
        }
    }

    const rawText = document.getText(targetRange);
    const normalized = normalizeForPrompt(rawText);
    if (!normalized.trim()) {
        return undefined;
    }

    const limited = truncateForPrompt(normalized, MAX_IDE_CONTEXT_CHARS);
    return {
        text: limited.text,
        range: targetRange,
        truncated: truncated || limited.truncated,
    };
}

function formatCodeBlock(
    languageId: string | undefined,
    content: string
): string {
    const language =
        languageId && languageId.trim().length > 0 ? languageId : "text";
    return `\`\`\`${language}\n${content}\n\`\`\``;
}

function getPathLabel(uri: vscode.Uri): string {
    if (uri.scheme === "untitled") {
        const segments = uri.path.split("/");
        const name = segments[segments.length - 1] || "untitled";
        return `untitled:${name}`;
    }

    const relative = vscode.workspace.asRelativePath(uri, false);
    if (relative && relative !== uri.toString()) {
        return relative;
    }

    if (uri.scheme === "file") {
        return uri.fsPath;
    }

    return uri.toString(true);
}

function formatRangeLabel(range: vscode.Range): string {
    const startLine = range.start.line + 1;
    const endLine = range.end.line + 1;
    return startLine === endLine ? `${startLine}` : `${startLine}-${endLine}`;
}

function createContextKey(
    uri: vscode.Uri | undefined,
    range: vscode.Range | undefined,
    fallback: string
): string {
    const base = uri ? uri.toString() : fallback;
    if (range) {
        return `${base}:${range.start.line}:${range.start.character}-${range.end.line}:${range.end.character}`;
    }
    return base;
}

function registerContextKey(seen: Set<string>, key: string): boolean {
    if (seen.has(key)) {
        return false;
    }
    seen.add(key);
    return true;
}

function normalizeForPrompt(text: string): string {
    return text.replace(/\r\n/g, "\n");
}

function truncateForPrompt(
    text: string,
    limit: number
): { text: string; truncated: boolean } {
    if (text.length <= limit) {
        return { text, truncated: false };
    }

    return { text: text.slice(0, limit), truncated: true };
}

function getIdeContextFilePath(): string | undefined {
    return ideContextBridge?.canonicalFilePath;
}

function getLegacyIdeContextFilePath(): string | undefined {
    return ideContextBridge?.legacyFilePath;
}

async function writeIdeContextTarget(
    target: IdeContextFileTarget,
    snapshotContent: string,
    markdownContent: string
): Promise<void> {
    if (target.directoryUri) {
        await vscode.workspace.fs.createDirectory(target.directoryUri);
    }

    await Promise.all([
        vscode.workspace.fs.writeFile(
            target.snapshotUri,
            Buffer.from(snapshotContent, "utf8")
        ),
        vscode.workspace.fs.writeFile(
            target.legacyMarkdownUri,
            Buffer.from(markdownContent, "utf8")
        ),
    ]);
}

function syncIdeContextEnvironmentVariable(): void {
    const canonicalPath = getIdeContextFilePath();
    if (canonicalPath) {
        process.env[CANONICAL_IDE_CONTEXT_ENV_VARIABLE] = canonicalPath;
    } else {
        // process.env is an object keyed by variable name, not an array.
        // ast-grep-ignore: no-array-delete
        delete process.env[CANONICAL_IDE_CONTEXT_ENV_VARIABLE];
    }

    const legacyPath = getLegacyIdeContextFilePath();
    if (legacyPath) {
        process.env[LEGACY_IDE_CONTEXT_ENV_VARIABLE] = legacyPath;
    } else {
        // process.env is an object keyed by variable name, not an array.
        // ast-grep-ignore: no-array-delete
        delete process.env[LEGACY_IDE_CONTEXT_ENV_VARIABLE];
    }
}

function isDocumentVisible(document: vscode.TextDocument): boolean {
    if (vscode.window.activeTextEditor?.document === document) {
        return true;
    }

    return vscode.window.visibleTextEditors.some(
        (editor) => editor.document === document
    );
}

class IdeContextFileBridge implements vscode.Disposable {
    private pendingTimer: NodeJS.Timeout | undefined;
    private currentRefresh: Promise<void> | undefined;
    private disposed = false;

    constructor(
        private readonly snapshotUri: vscode.Uri,
        private readonly legacyMarkdownUri: vscode.Uri
    ) {}

    dispose(): void {
        this.disposed = true;
        if (this.pendingTimer) {
            clearTimeout(this.pendingTimer);
            this.pendingTimer = undefined;
        }
    }

    scheduleRefresh(delay = 200): void {
        if (this.disposed) {
            return;
        }
        if (this.pendingTimer) {
            clearTimeout(this.pendingTimer);
        }
        this.pendingTimer = setTimeout(() => {
            this.pendingTimer = undefined;
            void this.performRefresh();
        }, delay);
    }

    async flush(): Promise<void> {
        if (this.disposed) {
            return;
        }
        if (this.pendingTimer) {
            clearTimeout(this.pendingTimer);
            this.pendingTimer = undefined;
        }
        await this.performRefresh();
    }

    get canonicalFilePath(): string | undefined {
        if (this.snapshotUri.scheme !== "file") {
            return undefined;
        }
        return this.snapshotUri.fsPath;
    }

    get legacyFilePath(): string | undefined {
        if (this.legacyMarkdownUri.scheme !== "file") {
            return undefined;
        }
        return this.legacyMarkdownUri.fsPath;
    }

    private async performRefresh(): Promise<void> {
        if (this.disposed) {
            return;
        }

        if (this.currentRefresh) {
            await this.currentRefresh;
            return;
        }

        const task = (async () => {
            try {
                const snapshot = buildIdeContextSnapshot();
                let block: string | undefined;
                try {
                    block = await buildIdeContextBlock({
                        includeActiveEditor: true,
                        includeVisibleEditors: true,
                    });
                } catch (error) {
                    const message =
                        error instanceof Error ? error.message : String(error);
                    getOutputChannel().appendLine(
                        `[warn] Failed to render legacy IDE context markdown: ${message}`
                    );
                }
                const snapshotContent = snapshot
                    ? `${JSON.stringify(snapshot, null, 2)}\n`
                    : "";
                const markdownContent = block ? `${block}\n` : "";
                const storageTarget: IdeContextFileTarget = {
                    snapshotUri: this.snapshotUri,
                    legacyMarkdownUri: this.legacyMarkdownUri,
                };
                await writeIdeContextTarget(
                    storageTarget,
                    snapshotContent,
                    markdownContent
                );
            } catch (error) {
                const message =
                    error instanceof Error ? error.message : String(error);
                getOutputChannel().appendLine(
                    `[warn] Failed to update IDE context snapshot: ${message}`
                );
            }
        })();

        this.currentRefresh = task;
        try {
            await task;
        } finally {
            if (this.currentRefresh === task) {
                this.currentRefresh = undefined;
            }
        }
    }
}

function getDiapasonEnvironment(
    overrides: NodeJS.ProcessEnv = {}
): Record<string, string> {
    const merged = { ...process.env, ...overrides };
    const env: Record<string, string> = {};
    for (const [key, value] of Object.entries(merged)) {
        if (typeof value === "string") {
            env[key] = value;
        }
    }

    const contextPath = getIdeContextFilePath();
    if (contextPath) {
        env[CANONICAL_IDE_CONTEXT_ENV_VARIABLE] = contextPath;
    } else {
        // env is a Record<string, string> keyed by variable name, not an array.
        // ast-grep-ignore: no-array-delete
        delete env[CANONICAL_IDE_CONTEXT_ENV_VARIABLE];
    }

    const legacyContextPath = getLegacyIdeContextFilePath();
    if (legacyContextPath) {
        env[LEGACY_IDE_CONTEXT_ENV_VARIABLE] = legacyContextPath;
    } else {
        // env is a Record<string, string> keyed by variable name, not an array.
        // ast-grep-ignore: no-array-delete
        delete env[LEGACY_IDE_CONTEXT_ENV_VARIABLE];
    }

    return env;
}

function getPrimaryWorkspaceFolder(): vscode.WorkspaceFolder | undefined {
    const activeEditor = vscode.window.activeTextEditor;
    if (activeEditor) {
        const folder = vscode.workspace.getWorkspaceFolder(
            activeEditor.document.uri
        );
        if (folder) {
            return folder;
        }
    }

    const [firstWorkspace] = vscode.workspace.workspaceFolders ?? [];
    return firstWorkspace;
}

function createSpawnOptions(
    overrides: Partial<SpawnOptionsWithoutStdio> = {}
): SpawnOptionsWithoutStdio {
    const { env: overrideEnv, ...rest } = overrides;
    return {
        env: getDiapasonEnvironment(overrideEnv ?? {}),
        ...rest,
    };
}

function formatArgsForLogging(args: string[]): string {
    return args
        .map((arg) => {
            const value = String(arg);
            return /(\s|"|')/.test(value) ? JSON.stringify(value) : value;
        })
        .join(" ");
}

function formatArgsForShell(args: string[]): string {
    return args
        .map((arg) => {
            const value = String(arg);
            return quoteForShell(value);
        })
        .filter((value) => value.length > 0)
        .join(" ");
}

function quoteForShell(value: string): string {
    if (!/[\s"'\\$`]/.test(value)) {
        return value;
    }

    return `"${value.replace(/(["\\$`])/g, "\\$1")}"`;
}

function getWorkspaceRoot(): string | undefined {
    const activeEditor = vscode.window.activeTextEditor;
    if (activeEditor) {
        const workspaceFolder = vscode.workspace.getWorkspaceFolder(
            activeEditor.document.uri
        );
        if (workspaceFolder) {
            return workspaceFolder.uri.fsPath;
        }
    }

    const [firstWorkspace] = vscode.workspace.workspaceFolders ?? [];
    return firstWorkspace?.uri.fsPath;
}

function handleCommandError(contextLabel: string, error: unknown) {
    const message = error instanceof Error ? error.message : String(error);
    void vscode.window.showErrorMessage(
        `Failed to ${contextLabel} with Diapason: ${message}`
    );
}

function getOutputChannel(): vscode.OutputChannel {
    if (!outputChannel) {
        outputChannel = vscode.window.createOutputChannel("Diapason");
    }

    return outputChannel;
}

async function openToolsPolicyGuide(): Promise<void> {
    const [guide] = await vscode.workspace.findFiles(
        "docs/diapason_tools_policy.md",
        "**/{node_modules,dist,out,.git,target}/**",
        1
    );
    if (guide) {
        const document = await vscode.workspace.openTextDocument(guide);
        await vscode.window.showTextDocument(document, { preview: false });
        return;
    }

    await vscode.env.openExternal(
        vscode.Uri.parse(
            "https://github.com/leynos/vtcode/blob/main/docs/modules/diapason_tools_policy.md"
        )
    );
}

async function openMcpGuide(): Promise<void> {
    const [guide] = await vscode.workspace.findFiles(
        "docs/guides/mcp-integration.md",
        "**/{node_modules,dist,out,.git,target}/**",
        1
    );
    if (guide) {
        const document = await vscode.workspace.openTextDocument(guide);
        await vscode.window.showTextDocument(document, { preview: false });
        return;
    }

    await vscode.env.openExternal(
        vscode.Uri.parse(
            "https://github.com/leynos/vtcode/blob/main/docs/guides/mcp-integration.md"
        )
    );
}

function ensureAgentTerminal(
    commandPath: string,
    cwd: string
): { terminal: vscode.Terminal; created: boolean } {
    if (agentTerminal) {
        return { terminal: agentTerminal, created: false };
    }

    const terminal = vscode.window.createTerminal({
        name: "Diapason Agent",
        cwd,
        env: getDiapasonEnvironment(),
        iconPath: new vscode.ThemeIcon("comment-discussion"),
    });

    // Instead of immediate execution, use a slight delay to allow any auto-activation to complete
    setTimeout(() => {
        void (async () => {
            if (ideContextBridge) {
                await ideContextBridge.flush();
            }
            const quotedCommandPath = /\s/.test(commandPath)
                ? `"${commandPath.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`
                : commandPath;
            const configArgs = getConfigArguments();
            const terminalArgs = ["chat", ...configArgs];
            const argsText = formatArgsForShell(terminalArgs);
            const commandText =
                argsText.length > 0
                    ? `${quotedCommandPath} ${argsText}`
                    : quotedCommandPath;
            // Send the Diapason command after a brief delay to allow any environment activation to complete
            terminal.sendText(commandText, true);
        })();
    }, 800); // 800ms delay to allow environment activation if it happens

    agentTerminal = terminal;

    if (!terminalCloseListener) {
        terminalCloseListener = vscode.window.onDidCloseTerminal((closed) => {
            if (closed === agentTerminal) {
                agentTerminal = undefined;
                terminalCloseListener?.dispose();
                terminalCloseListener = undefined;
            }
        });
    }

    return { terminal, created: true };
}

function ensureStableApi(context: vscode.ExtensionContext) {
    const manifest = context.extension.packageJSON as
        | { enabledApiProposals?: string[] }
        | undefined;
    const proposals = manifest?.enabledApiProposals ?? [];

    if (proposals.length > 0) {
        const channel = getOutputChannel();
        channel.appendLine(
            `[warn] Proposed VS Code APIs enabled: ${proposals.join(", ")}.`
        );
    }
}

function logExtensionHostContext(context: vscode.ExtensionContext) {
    const channel = getOutputChannel();
    const remoteName = vscode.env.remoteName
        ? `remote (${vscode.env.remoteName})`
        : "local";
    const hostKind =
        vscode.env.uiKind === vscode.UIKind.Web ? "web" : "desktop";
    const modeLabel = getExtensionModeLabel(context.extensionMode);
    channel.appendLine(
        `[info] Diapason Companion activated in ${remoteName} ${hostKind} host (${modeLabel} mode).`
    );
}

function getExtensionModeLabel(mode: vscode.ExtensionMode): string {
    switch (mode) {
        case vscode.ExtensionMode.Development:
            return "development";
        case vscode.ExtensionMode.Test:
            return "test";
        case vscode.ExtensionMode.Production:
        default:
            return "production";
    }
}
