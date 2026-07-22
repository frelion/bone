import { type TSchema, Type } from "typebox";
import type { AgentToolResult, ToolDefinition } from "../extensions/types.ts";
import { wrapToolDefinitions } from "../tools/tool-definition-wrapper.ts";
import { boundedForgeToolResult, MAX_FORGE_BATCH_IDS, MAX_FORGE_QUERY_LIMIT } from "./result.ts";
import { createForgeService } from "./service.ts";

export const FORGE_READ_TOOL_NAMES = ["forge_context", "forge_query", "forge_audit", "forge_watch"] as const;
export const FORGE_WRITE_TOOL_NAMES = [
	"forge_issue",
	"forge_milestone",
	"forge_change",
	"forge_wiki",
	"forge_pipeline",
	"forge_release",
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

/** Platform-independent boundary used by the built-in tools and SDK tests. */
export interface ForgeService {
	execute(
		toolName: ForgeToolName,
		input: Record<string, unknown>,
		signal: AbortSignal | undefined,
		context: ForgeToolContext,
	): Promise<unknown>;
}

export interface CreateForgeToolDefinitionsOptions {
	cwd: string;
	agentDir?: string;
	service?: ForgeService;
}

const repositoryFields = {
	remote: Type.Optional(
		Type.String({ description: "Git remote name or URL; defaults to the current repository remote" }),
	),
	project: Type.Optional(
		Type.String({ description: "Explicit namespace/project path when remote discovery is unsuitable" }),
	),
};

const contextSchema = Type.Object({
	...repositoryFields,
	refresh: Type.Optional(Type.Boolean({ description: "Bypass cached version and capability information" })),
});

const querySchema = Type.Object({
	...repositoryFields,
	resource: Type.Union(
		[
			Type.Literal("issue"),
			Type.Literal("milestone"),
			Type.Literal("change"),
			Type.Literal("wiki"),
			Type.Literal("pipeline"),
			Type.Literal("job"),
			Type.Literal("release"),
		],
		{ description: "Resource type to retrieve" },
	),
	id: Type.Optional(
		Type.Union([Type.String(), Type.Number()], {
			description: "Retrieve one bounded detail record instead of a list",
		}),
	),
	ids: Type.Optional(
		Type.Array(Type.Union([Type.String(), Type.Number()]), {
			minItems: 1,
			maxItems: MAX_FORGE_BATCH_IDS,
			description: "Retrieve 1 to 5 bounded detail records for comparison",
		}),
	),
	parentId: Type.Optional(
		Type.Union([Type.String(), Type.Number()], { description: "Parent pipeline or workflow run id for job lists" }),
	),
	state: Type.Optional(Type.String()),
	search: Type.Optional(
		Type.String({
			minLength: 1,
			maxLength: 512,
			description: "Provider-side text search within the current repository",
		}),
	),
	cursor: Type.Optional(Type.String({ description: "Opaque nextCursor from the preceding list result" })),
	limit: Type.Optional(
		Type.Integer({
			minimum: 1,
			maximum: MAX_FORGE_QUERY_LIMIT,
			description: "List size; defaults to 10 and cannot exceed 50",
		}),
	),
});

const auditSchema = Type.Object({
	...repositoryFields,
	workflow: Type.Optional(
		Type.Union([
			Type.Literal("current"),
			Type.Literal("submit_review"),
			Type.Literal("merge"),
			Type.Literal("release"),
		]),
	),
	changeId: Type.Optional(Type.Union([Type.String(), Type.Number()])),
	input: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
	refresh: Type.Optional(Type.Boolean()),
});

const watchSchema = Type.Object({
	...repositoryFields,
	resource: Type.Union([
		Type.Literal("pipeline"),
		Type.Literal("job"),
		Type.Literal("change"),
		Type.Literal("review"),
	]),
	id: Type.Union([Type.String(), Type.Number()]),
	until: Type.Array(Type.String(), { minItems: 1, description: "Terminal states that complete the wait" }),
	timeoutSeconds: Type.Optional(Type.Number({ minimum: 1, maximum: 3600 })),
	pollIntervalSeconds: Type.Optional(Type.Number({ minimum: 1, maximum: 60 })),
});

const mutationBase = {
	...repositoryFields,
	action: Type.String({ description: "Resource-specific mutation action" }),
	id: Type.Optional(Type.Union([Type.String(), Type.Number()])),
	requestId: Type.String({ description: "Stable idempotency key for this requested mutation" }),
	input: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
};

const mutationSchema = Type.Object(mutationBase);
const transitionSchema = Type.Object({
	...repositoryFields,
	transition: Type.Union([
		Type.Literal("submit_review"),
		Type.Literal("prepare_merge"),
		Type.Literal("merge"),
		Type.Literal("prepare_release"),
		Type.Literal("release"),
	]),
	requestId: Type.Optional(Type.String({ description: "Required for transitions that perform mutations" })),
	issueId: Type.Optional(Type.Union([Type.String(), Type.Number()])),
	changeId: Type.Optional(Type.Union([Type.String(), Type.Number()])),
	milestoneId: Type.Optional(Type.Union([Type.String(), Type.Number()])),
	input: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
});

interface ForgeToolSpec {
	name: ForgeToolName;
	label: string;
	description: string;
	promptSnippet: string;
	parameters: TSchema;
}

const specs: ForgeToolSpec[] = [
	{
		name: "forge_context",
		label: "Forge context",
		description:
			"Resolve the GitLab or GitHub repository, instance version, identity, and negotiated read capabilities.",
		promptSnippet: "Inspect repository hosting context and capabilities",
		parameters: contextSchema,
	},
	{
		name: "forge_query",
		label: "Forge query",
		description:
			"Read issues, milestones, merge requests or pull requests, wiki pages, pipelines, jobs, and releases.",
		promptSnippet: "Query GitLab or GitHub resources",
		parameters: querySchema,
	},
	{
		name: "forge_audit",
		label: "Forge audit",
		description:
			"Evaluate the repository workflow policy against current local and remote state without changing either.",
		promptSnippet: "Audit development workflow policy",
		parameters: auditSchema,
	},
	{
		name: "forge_watch",
		label: "Forge watch",
		description: "Wait for pipeline, job, review, or change-request state with bounded, cancellable polling.",
		promptSnippet: "Wait for remote development state",
		parameters: watchSchema,
	},
	{
		name: "forge_issue",
		label: "Forge issue",
		description:
			"Create or update issues, comments, assignments, labels, and milestone associations subject to repository policy.",
		promptSnippet: "Manage issues on GitLab or GitHub",
		parameters: mutationSchema,
	},
	{
		name: "forge_milestone",
		label: "Forge milestone",
		description: "Create, update, close, or reopen milestones subject to repository policy.",
		promptSnippet: "Manage development milestones",
		parameters: mutationSchema,
	},
	{
		name: "forge_change",
		label: "Forge change",
		description:
			"Manage merge requests or pull requests, reviews, discussions, approvals, and merges subject to policy and approval gates.",
		promptSnippet: "Manage merge requests or pull requests",
		parameters: mutationSchema,
	},
	{
		name: "forge_wiki",
		label: "Forge wiki",
		description: "Create, update, or delete supported project wiki content subject to policy and capability checks.",
		promptSnippet: "Manage project wiki content",
		parameters: mutationSchema,
	},
	{
		name: "forge_pipeline",
		label: "Forge pipeline",
		description: "Trigger, retry, cancel, or run supported pipelines and jobs subject to policy and approval gates.",
		promptSnippet: "Manage CI pipelines and jobs",
		parameters: mutationSchema,
	},
	{
		name: "forge_release",
		label: "Forge release",
		description: "Create or update releases and related tags subject to policy and approval gates.",
		promptSnippet: "Manage project releases",
		parameters: mutationSchema,
	},
	{
		name: "forge_transition",
		label: "Forge workflow",
		description: "Run a governed workflow transition such as submitting review, merging, or releasing.",
		promptSnippet: "Run a governed development workflow transition",
		parameters: transitionSchema,
	},
];

export function createForgeToolDefinitions(
	options: CreateForgeToolDefinitionsOptions,
): Record<ForgeToolName, ToolDefinition> {
	const service = options.service ?? createForgeService({ cwd: options.cwd, agentDir: options.agentDir });

	return Object.fromEntries(
		specs.map((spec) => {
			const definition: ToolDefinition = {
				name: spec.name,
				label: spec.label,
				description: spec.description,
				promptSnippet: spec.promptSnippet,
				parameters: spec.parameters,
				executionMode: FORGE_WRITE_TOOL_NAMES.includes(spec.name as ForgeWriteToolName) ? "sequential" : "parallel",
				async execute(toolCallId, input, signal, _onUpdate, ctx): Promise<AgentToolResult<unknown>> {
					const extensionContext = ctx as typeof ctx | undefined;
					const result = await service.execute(spec.name, input as Record<string, unknown>, signal, {
						cwd: options.cwd,
						agentDir: options.agentDir,
						toolCallId,
						interactive: extensionContext?.hasUI ?? false,
						projectTrusted: extensionContext?.isProjectTrusted() ?? false,
						confirm: (title, message) =>
							extensionContext?.uiV2?.dialogs.confirm({ title, message }) ?? Promise.resolve(false),
					});
					const bounded = boundedForgeToolResult(result);
					return { content: [{ type: "text", text: bounded.text }], details: bounded.value };
				},
			};
			return [spec.name, definition];
		}),
	) as Record<ForgeToolName, ToolDefinition>;
}

export function createForgeTools(options: CreateForgeToolDefinitionsOptions) {
	return wrapToolDefinitions(Object.values(createForgeToolDefinitions(options)));
}
