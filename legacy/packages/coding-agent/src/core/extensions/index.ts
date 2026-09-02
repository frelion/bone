/**
 * Extension system for lifecycle events and custom tools.
 */

export type { SlashCommandInfo, SlashCommandSource } from "../slash-commands.ts";
export type { SourceInfo } from "../source-info.ts";
export {
	createExtensionRuntime,
	discoverAndLoadExtensions,
	loadExtensionFromFactory,
	loadExtensions,
} from "./loader.ts";
export type {
	ExtensionErrorListener,
	ForkHandler,
	NavigateTreeHandler,
	NewSessionHandler,
	ShutdownHandler,
	SwitchSessionHandler,
} from "./runner.ts";
export { ExtensionRunner } from "./runner.ts";
export type {
	AfterProviderResponseEvent,
	AgentEndEvent,
	AgentSettledEvent,
	AgentStartEvent,
	// Re-exports
	AgentToolResult,
	AgentToolUpdateCallback,
	AppendEntryHandler,
	// Events - Tool (ToolCallEvent types)
	BashToolCallEvent,
	BashToolResultEvent,
	BeforeAgentStartEvent,
	BeforeAgentStartEventResult,
	BeforeProviderHeadersEvent,
	BeforeProviderRequestEvent,
	BeforeProviderRequestEventResult,
	BuildSystemPromptOptions,
	// Context
	CompactOptions,
	// Events - Agent
	ContextEvent,
	// Event Results
	ContextEventResult,
	ContextUsage,
	// Message and Entry Rendering
	CustomEntryViewRenderer,
	CustomMessageViewRenderer,
	CustomToolCallEvent,
	CustomToolResultEvent,
	EditToolCallEvent,
	EditToolResultEvent,
	EntryViewOptions,
	ExecOptions,
	ExecResult,
	Extension,
	ExtensionActions,
	// API
	ExtensionAPI,
	ExtensionCommandContext,
	ExtensionCommandContextActions,
	ExtensionContext,
	ExtensionContextActions,
	// Errors
	ExtensionError,
	ExtensionEvent,
	ExtensionFactory,
	ExtensionFlag,
	ExtensionHandler,
	ExtensionMode,
	// Runtime
	ExtensionRuntime,
	FindToolCallEvent,
	FindToolResultEvent,
	GetActiveToolsHandler,
	GetAllToolsHandler,
	GetCommandsHandler,
	GetThinkingLevelHandler,
	GrepToolCallEvent,
	GrepToolResultEvent,
	InlineExtension,
	// Events - Input
	InputEvent,
	InputEventResult,
	InputSource,
	LoadExtensionsResult,
	LsToolCallEvent,
	LsToolResultEvent,
	// Events - Message
	MessageEndEvent,
	MessageStartEvent,
	MessageUpdateEvent,
	MessageViewOptions,
	ModelSelectEvent,
	ModelSelectSource,
	ProjectTrustContext,
	ProjectTrustEvent,
	ProjectTrustEventDecision,
	ProjectTrustEventResult,
	ProjectTrustHandler,
	// Provider Registration
	ProviderConfig,
	ProviderModelConfig,
	ReadToolCallEvent,
	ReadToolResultEvent,
	// Commands
	RegisteredCommand,
	RegisteredTool,
	ReplacedSessionContext,
	ResolvedCommand,
	// Events - Resources
	ResourcesDiscoverEvent,
	ResourcesDiscoverResult,
	SendMessageHandler,
	SendUserMessageHandler,
	SessionBeforeCompactEvent,
	SessionBeforeCompactResult,
	SessionBeforeForkEvent,
	SessionBeforeForkResult,
	SessionBeforeSwitchEvent,
	SessionBeforeSwitchResult,
	SessionBeforeTreeEvent,
	SessionBeforeTreeResult,
	SessionCompactEvent,
	SessionEvent,
	SessionInfoChangedEvent,
	SessionShutdownEvent,
	// Events - Session
	SessionStartEvent,
	SessionTreeEvent,
	SetActiveToolsHandler,
	SetLabelHandler,
	SetModelHandler,
	SetThinkingLevelHandler,
	// Events - Tool
	ToolCallEvent,
	ToolCallEventResult,
	// Tools
	ToolDefinition,
	// Events - Tool Execution
	ToolExecutionEndEvent,
	// Tool execution mode
	ToolExecutionMode,
	ToolExecutionStartEvent,
	ToolExecutionUpdateEvent,
	ToolInfo,
	ToolResultEvent,
	ToolResultEventResult,
	TreePreparation,
	TurnEndEvent,
	TurnStartEvent,
	// Events - User Bash
	UserBashEvent,
	UserBashEventResult,
	WriteToolCallEvent,
	WriteToolResultEvent,
} from "./types.ts";
// Type guards
export {
	defineTool,
	isBashToolResult,
	isEditToolResult,
	isFindToolResult,
	isGrepToolResult,
	isLsToolResult,
	isReadToolResult,
	isToolCallEventType,
	isWriteToolResult,
} from "./types.ts";
export type {
	ExtensionUIAdvancedOptions,
	ExtensionUIAdvancedService,
	ExtensionUIAdvancedViewContext,
	ExtensionUIChromeService,
	ExtensionUIConfirmRequest,
	ExtensionUIDialogOptionsV2,
	ExtensionUIDialogService,
	ExtensionUIEditorService,
	ExtensionUIInputRequest,
	ExtensionUINotificationKind,
	ExtensionUISelectOption,
	ExtensionUISelectRequest,
	ExtensionUIToolResultService,
	ExtensionUIToolResultViewInput,
	ExtensionUIToolViewRenderer,
	ExtensionUIToolViewState,
	ExtensionUIV2Context,
	ExtensionUIView,
	ExtensionUIViewFactory,
	ExtensionUIViewHandle,
	ExtensionUIWidgetOptionsV2,
	ExtensionUIWidgetPlacement,
	ExtensionUIWidgetService,
} from "./ui-v2.ts";
export { createExtensionUIV2Context, resolveExtensionUIV2 } from "./ui-v2.ts";
export { wrapRegisteredTool, wrapRegisteredTools } from "./wrapper.ts";
