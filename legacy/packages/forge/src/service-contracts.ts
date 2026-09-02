export const FORGE_READ_TOOL_NAMES = ["forge_context", "forge_query", "forge_audit", "forge_watch"] as const;
export const FORGE_WRITE_TOOL_NAMES = [
	"forge_issue",
	"forge_milestone",
	"forge_change",
	"forge_wiki",
	"forge_pipeline",
	"forge_release",
	"forge_variable",
	"forge_transition",
] as const;
export const FORGE_TOOL_NAMES = [...FORGE_READ_TOOL_NAMES, ...FORGE_WRITE_TOOL_NAMES] as const;

export type ForgeReadToolName = (typeof FORGE_READ_TOOL_NAMES)[number];
export type ForgeWriteToolName = (typeof FORGE_WRITE_TOOL_NAMES)[number];
export type ForgeToolName = (typeof FORGE_TOOL_NAMES)[number];

export interface ForgeToolContext {
	cwd: string;
	agentDir?: string;
	toolCallId: string;
	interactive: boolean;
	projectTrusted: boolean;
	confirm(title: string, message: string): Promise<boolean>;
}

export interface ForgeService {
	execute(
		toolName: ForgeToolName,
		input: Record<string, unknown>,
		signal: AbortSignal | undefined,
		context: ForgeToolContext,
	): Promise<unknown>;
	close?(): Promise<void>;
}
