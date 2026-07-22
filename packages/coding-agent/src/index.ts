// Core session management

export { type Args, parseArgs } from "./cli/args.ts";

// Config paths
export {
	CONFIG_DIR_NAME,
	getAgentDir,
	getDocsPath,
	getExamplesPath,
	getPackageDir,
	getReadmePath,
	VERSION,
} from "./config.ts";
export {
	AgentSession,
	type AgentSessionConfig,
	type AgentSessionEvent,
	type AgentSessionEventListener,
	type ModelCycleResult,
	type ParsedSkillBlock,
	type PromptOptions,
	parseSkillBlock,
	type SessionStats,
} from "./core/agent-session.ts";
export { readStoredCredential } from "./core/auth-storage.ts";
// Compaction
export {
	type BranchPreparation,
	type BranchSummaryResult,
	type CollectEntriesResult,
	type CompactionResult,
	type CutPointResult,
	calculateContextTokens,
	collectEntriesForBranchSummary,
	compact,
	DEFAULT_COMPACTION_SETTINGS,
	estimateTokens,
	type FileOperations,
	findCutPoint,
	findTurnStartIndex,
	type GenerateBranchSummaryOptions,
	generateBranchSummary,
	generateSummary,
	getLastAssistantUsage,
	prepareBranchEntries,
	serializeConversation,
	shouldCompact,
} from "./core/compaction/index.ts";
export { createEventBus, type EventBus, type EventBusController } from "./core/event-bus.ts";
// Extension runtime types and factories are internal Bone implementation details.
// Footer data provider (git branch + extension statuses - data not otherwise available to extensions)
export type { ReadonlyFooterDataProvider } from "./core/footer-data-provider.ts";
export { convertToLlm } from "./core/messages.ts";
export { ModelRegistry } from "./core/model-registry.ts";
export {
	type ModelScopeDiagnostic,
	type ResolveCliModelResult,
	type ResolveModelScopeResult,
	resolveCliModel,
	resolveModelScopeWithDiagnostics,
	type ScopedModel,
} from "./core/model-resolver.ts";
export {
	type CreateModelRuntimeOptions,
	ModelRuntime,
	type ModelRuntimeAuthOverrides,
} from "./core/model-runtime.ts";
export {
	appendPlanModeInstructions,
	type CollaborationMode,
	type PlanDecision,
	type PlanProposal,
	type PlanState,
	parseProposedPlan,
} from "./core/plan-mode.ts";
export {
	type AskUserQuestionInput,
	askUserQuestionSchema,
	createAskUserQuestionToolDefinition,
	createCancelledQuestionToolResult,
	createQuestionToolResult,
	MAX_QUESTION_HEADER_LENGTH,
	MAX_QUESTION_OPTIONS,
	MAX_QUESTIONS,
	MIN_QUESTION_OPTIONS,
	type QuestionAnswer,
	type QuestionCancelReason,
	type QuestionDefinition,
	type QuestionOption,
	type QuestionRequest,
	type QuestionState,
	type QuestionToolDetails,
	RESERVED_QUESTION_OPTION_LABELS,
	validateQuestionAnswers,
	validateQuestionDefinitions,
} from "./core/question.ts";
export type { ResourceCollision, ResourceDiagnostic, ResourceLoader } from "./core/resource-loader.ts";
export { DefaultResourceLoader, loadProjectContextFiles } from "./core/resource-loader.ts";
// SDK for programmatic usage
export {
	AgentSessionRuntime,
	type AgentSessionRuntimeDiagnostic,
	type AgentSessionServices,
	type CreateAgentSessionFromServicesOptions,
	type CreateAgentSessionOptions,
	type CreateAgentSessionResult,
	type CreateAgentSessionRuntimeFactory,
	type CreateAgentSessionRuntimeResult,
	type CreateAgentSessionServicesOptions,
	// Factory
	createAgentSession,
	createAgentSessionFromServices,
	createAgentSessionRuntime,
	createAgentSessionServices,
	createBashTool,
	// Tool factories (for custom cwd)
	createCodingTools,
	createEditTool,
	createFindTool,
	createGrepTool,
	createLsTool,
	createReadOnlyTools,
	createReadTool,
	createWriteTool,
	type PromptTemplate,
} from "./core/sdk.ts";
export {
	type BranchSummaryEntry,
	buildContextEntries,
	buildSessionContext,
	type CollaborationModeChangeEntry,
	type CompactionEntry,
	CURRENT_SESSION_VERSION,
	type CustomEntry,
	type CustomMessageEntry,
	type FileEntry,
	getLatestCompactionEntry,
	type ModelChangeEntry,
	migrateSessionEntries,
	type NewSessionOptions,
	type PlanDecisionEntry,
	type PlanProposalEntry,
	parseSessionEntries,
	type SessionContext,
	type SessionEntry,
	type SessionEntryBase,
	type SessionHeader,
	type SessionInfo,
	type SessionInfoEntry,
	SessionManager,
	type SessionMessageEntry,
	type SessionTreeNode,
	sessionEntryToContextMessages,
	type ThinkingLevelChangeEntry,
} from "./core/session-manager.ts";
export {
	type CompactionSettings,
	type DefaultProjectTrust,
	type ImageSettings,
	type RetrySettings,
	SettingsManager,
	type SettingsManagerCreateOptions,
} from "./core/settings-manager.ts";
// Skills
export {
	formatSkillsForPrompt,
	type LoadSkillsFromDirOptions,
	type LoadSkillsResult,
	loadSkills,
	loadSkillsFromDir,
	type Skill,
	type SkillFrontmatter,
} from "./core/skills.ts";
export { createSyntheticSourceInfo } from "./core/source-info.ts";
export { type EditDiffResult, generateDiffString, generateUnifiedPatch } from "./core/tools/edit-diff.ts";
// Tools
export {
	type BashOperations,
	type BashSpawnContext,
	type BashSpawnHook,
	type BashToolDetails,
	type BashToolInput,
	type BashToolOptions,
	type CreateForgeToolDefinitionsOptions,
	createBashToolDefinition,
	createEditToolDefinition,
	createFindToolDefinition,
	createForgeToolDefinitions,
	createForgeTools,
	createGrepToolDefinition,
	createLocalBashOperations,
	createLsToolDefinition,
	createReadToolDefinition,
	createWriteToolDefinition,
	DEFAULT_MAX_BYTES,
	DEFAULT_MAX_LINES,
	type EditOperations,
	type EditToolDetails,
	type EditToolInput,
	type EditToolOptions,
	type FindOperations,
	type FindToolDetails,
	type FindToolInput,
	type FindToolOptions,
	FORGE_READ_TOOL_NAMES,
	FORGE_TOOL_NAMES,
	FORGE_WRITE_TOOL_NAMES,
	type ForgeService,
	type ForgeToolContext,
	type ForgeToolName,
	formatSize,
	type GrepOperations,
	type GrepToolDetails,
	type GrepToolInput,
	type GrepToolOptions,
	type LsOperations,
	type LsToolDetails,
	type LsToolInput,
	type LsToolOptions,
	type ReadOperations,
	type ReadToolDetails,
	type ReadToolInput,
	type ReadToolOptions,
	type ToolsOptions,
	type TruncationOptions,
	type TruncationResult,
	truncateHead,
	truncateLine,
	truncateTail,
	type WriteOperations,
	type WriteToolInput,
	type WriteToolOptions,
	withFileMutationQueue,
} from "./core/tools/index.ts";
export {
	hasTrustRequiringProjectResources,
	type ProjectTrustDecision,
	ProjectTrustStore,
	type ProjectTrustStoreEntry,
	type ProjectTrustUpdate,
} from "./core/trust-manager.ts";
// Main entry point
export { type MainOptions, main } from "./main.ts";
// Run modes for programmatic SDK usage
export {
	InteractiveMode,
	type InteractiveModeOptions,
	type ModelInfo,
	type PrintModeOptions,
	RpcClient,
	type RpcClientOptions,
	type RpcCommand,
	type RpcEventListener,
	type RpcExtensionUIRequest,
	type RpcExtensionUIResponse,
	type RpcResponse,
	type RpcSessionState,
	runPrintMode,
	runRpcMode,
} from "./modes/index.ts";
// UI components for extensions
export {
	ArminComponent,
	AssistantMessageComponent,
	BashExecutionComponent,
	BorderedLoader,
	BranchSummaryMessageComponent,
	CompactionSummaryMessageComponent,
	CustomEditor,
	CustomMessageComponent,
	DynamicBorder,
	ExtensionEditorComponent,
	ExtensionInputComponent,
	ExtensionSelectorComponent,
	FooterComponent,
	keyHint,
	keyText,
	LoginDialogComponent,
	ModelSelectorComponent,
	OAuthSelectorComponent,
	type RenderDiffOptions,
	rawKeyHint,
	renderDiff,
	SessionSelectorComponent,
	ShowImagesSelectorComponent,
	SkillInvocationMessageComponent,
	ThemeSelectorComponent,
	ThinkingSelectorComponent,
	ToolExecutionComponent,
	type ToolExecutionOptions,
	TreeSelectorComponent,
	truncateToVisualLines,
	UserMessageComponent,
	UserMessageSelectorComponent,
	type VisualTruncateResult,
} from "./modes/interactive/components/index.ts";
// Theme utilities for custom tools and extensions
export {
	getLanguageFromPath,
	getMarkdownTheme,
	getSelectListTheme,
	getSettingsListTheme,
	highlightCode,
	initTheme,
	Theme,
	type ThemeColor,
} from "./modes/interactive/theme/theme.ts";
// Clipboard utilities
export { copyToClipboard } from "./utils/clipboard.ts";
export { parseFrontmatter, stripFrontmatter } from "./utils/frontmatter.ts";
export { convertToPng } from "./utils/image-convert.ts";
export { formatDimensionNote, type ResizedImage, resizeImage } from "./utils/image-resize.ts";
// Shell utilities
export { getShellConfig } from "./utils/shell.ts";
