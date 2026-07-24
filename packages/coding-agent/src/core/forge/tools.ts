import { type TSchema, Type } from "typebox";
import type { AgentToolResult, ToolDefinition } from "../extensions/types.ts";
import { type AgentToolContract, defineAgentToolContract } from "../tools/agent-tool-contract.ts";
import { wrapToolDefinitions } from "../tools/tool-definition-wrapper.ts";
import { ForgeError } from "./errors.ts";
import {
	boundedForgeToolResult,
	MAX_FORGE_BATCH_IDS,
	MAX_FORGE_QUERY_LIMIT,
	MAX_FORGE_TOOL_RESULT_BYTES,
	projectForgeMutation,
} from "./result.ts";
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
	close?(): Promise<void>;
}

export interface CreateForgeToolDefinitionsOptions {
	cwd: string;
	agentDir?: string;
	service?: ForgeService;
}

const closed = { additionalProperties: false } as const;
const nonEmptyClosed = { ...closed, minProperties: 1 } as const;
const nonEmptyString = (description: string, maximum = 512) =>
	Type.String({ minLength: 1, maxLength: maximum, description });
const positiveInteger = (description: string) => Type.Integer({ minimum: 1, description });
const resourceSchema = Type.Union([
	Type.Literal("issue"),
	Type.Literal("milestone"),
	Type.Literal("change"),
	Type.Literal("wiki"),
	Type.Literal("pipeline"),
	Type.Literal("job"),
	Type.Literal("release"),
]);
const resourceIdentifier = Type.Union([
	positiveInteger("Positive numeric resource identifier"),
	nonEmptyString("Wiki slug or release tag", 256),
]);
const requestId = nonEmptyString("Stable idempotency key; reuse only for the exact same mutation", 128);
const shortText = (description: string) => nonEmptyString(description, 256);
const bodyText = (description: string) => nonEmptyString(description, 64 * 1024);
const stringList = (description: string) =>
	Type.Array(nonEmptyString(description, 128), { minItems: 1, maxItems: 50, description });

const contextSchema = Type.Object({}, closed);

const paginationFields = {
	cursor: Type.Optional(nonEmptyString("Opaque nextCursor returned by the preceding list call", 256)),
	limit: Type.Optional(
		Type.Integer({ minimum: 1, maximum: MAX_FORGE_QUERY_LIMIT, description: "Defaults to 10; maximum 50" }),
	),
};
const openState = Type.Optional(
	Type.Union([Type.Literal("open"), Type.Literal("closed"), Type.Literal("all")], {
		description: "Provider-neutral state filter; omit when not needed",
	}),
);
const pipelineState = Type.Optional(
	Type.Union(
		[
			Type.Literal("running"),
			Type.Literal("pending"),
			Type.Literal("success"),
			Type.Literal("failed"),
			Type.Literal("canceled"),
			Type.Literal("skipped"),
		],
		{ description: "Provider-neutral pipeline status filter; omit when not needed" },
	),
);
const querySchema = Type.Union([
	Type.Object(
		{
			operation: Type.Literal("list"),
			resource: Type.Union([Type.Literal("issue"), Type.Literal("milestone"), Type.Literal("change")]),
			state: openState,
			search: Type.Optional(nonEmptyString("Repository-scoped provider-side text search")),
			...paginationFields,
		},
		closed,
	),
	Type.Object(
		{
			operation: Type.Literal("list"),
			resource: Type.Literal("pipeline"),
			state: pipelineState,
			...paginationFields,
		},
		closed,
	),
	Type.Object(
		{
			operation: Type.Literal("list"),
			resource: Type.Literal("job"),
			parentId: positiveInteger("Pipeline or workflow run id"),
			...paginationFields,
		},
		closed,
	),
	Type.Object(
		{
			operation: Type.Literal("list"),
			resource: Type.Union([Type.Literal("wiki"), Type.Literal("release")]),
			...paginationFields,
		},
		closed,
	),
	Type.Object(
		{
			operation: Type.Literal("get"),
			resource: resourceSchema,
			id: resourceIdentifier,
		},
		closed,
	),
	Type.Object(
		{
			operation: Type.Literal("get_many"),
			resource: resourceSchema,
			ids: Type.Array(resourceIdentifier, {
				minItems: 1,
				maxItems: MAX_FORGE_BATCH_IDS,
				description: "One to five unique identifiers for bounded comparison",
			}),
		},
		closed,
	),
]);

const releaseAuditInputSchema = Type.Object(
	{
		ref: Type.Optional(shortText("Target branch or commit")),
		milestoneTitles: Type.Optional(stringList("Release milestone title")),
	},
	closed,
);
const auditSchema = Type.Union([
	Type.Object({ workflow: Type.Literal("current") }, closed),
	Type.Object({ workflow: Type.Literal("submit_review") }, closed),
	Type.Object(
		{ workflow: Type.Literal("merge"), changeId: positiveInteger("Pull request or merge request number") },
		closed,
	),
	Type.Object({ workflow: Type.Literal("release"), input: Type.Optional(releaseAuditInputSchema) }, closed),
]);

const watchSchema = Type.Object(
	{
		resource: Type.Union([
			Type.Literal("pipeline"),
			Type.Literal("job"),
			Type.Literal("change"),
			Type.Literal("review"),
		]),
		id: positiveInteger("Pipeline, job, change, or review identifier"),
		until: Type.Array(nonEmptyString("Terminal state that completes the wait", 64), {
			minItems: 1,
			maxItems: 8,
			description: "Terminal states returned by Forge; never use empty placeholders",
		}),
		timeoutSeconds: Type.Optional(Type.Integer({ minimum: 1, maximum: 3600 })),
		pollIntervalSeconds: Type.Optional(Type.Integer({ minimum: 1, maximum: 60 })),
	},
	closed,
);

const issueFields = {
	title: Type.Optional(shortText("Issue title")),
	body: Type.Optional(bodyText("Issue body")),
	labels: Type.Optional(stringList("Issue label")),
	assignees: Type.Optional(stringList("Assignee username")),
	milestoneNumber: Type.Optional(positiveInteger("Milestone number")),
};
const milestoneFields = {
	title: Type.Optional(shortText("Milestone title")),
	description: Type.Optional(bodyText("Milestone description")),
	dueDate: Type.Optional(Type.String({ format: "date", description: "Due date in YYYY-MM-DD format" })),
};
const changeUpdateFields = {
	title: Type.Optional(shortText("Pull request or merge request title")),
	body: Type.Optional(bodyText("Pull request or merge request body")),
	targetBranch: Type.Optional(shortText("Target branch")),
};

function mutationVariant(action: string, identifier: Record<string, TSchema> = {}, input?: TSchema): TSchema {
	return Type.Object(
		{
			action: Type.Literal(action),
			...identifier,
			requestId,
			...(input ? { input } : {}),
		},
		closed,
	);
}

const issueMutationSchema = Type.Union([
	mutationVariant("create", {}, Type.Object({ ...issueFields, title: shortText("Issue title") }, closed)),
	mutationVariant(
		"update",
		{ issueNumber: positiveInteger("Issue number") },
		Type.Object(issueFields, nonEmptyClosed),
	),
	mutationVariant(
		"comment",
		{ issueNumber: positiveInteger("Issue number") },
		Type.Object({ body: bodyText("Comment body") }, closed),
	),
	mutationVariant("close", { issueNumber: positiveInteger("Issue number") }),
	mutationVariant("reopen", { issueNumber: positiveInteger("Issue number") }),
]);

const milestoneMutationSchema = Type.Union([
	mutationVariant("create", {}, Type.Object({ ...milestoneFields, title: shortText("Milestone title") }, closed)),
	mutationVariant(
		"update",
		{ milestoneNumber: positiveInteger("Milestone number") },
		Type.Object(milestoneFields, nonEmptyClosed),
	),
	mutationVariant("close", { milestoneNumber: positiveInteger("Milestone number") }),
	mutationVariant("reopen", { milestoneNumber: positiveInteger("Milestone number") }),
]);

const changeMutationSchema = Type.Union([
	mutationVariant(
		"create",
		{},
		Type.Object(
			{
				...changeUpdateFields,
				title: shortText("Pull request or merge request title"),
				sourceBranch: shortText("Source branch"),
				targetBranch: shortText("Target branch"),
				draft: Type.Optional(Type.Boolean({ description: "Create as draft" })),
			},
			closed,
		),
	),
	mutationVariant(
		"update",
		{ changeNumber: positiveInteger("Change number") },
		Type.Object(changeUpdateFields, nonEmptyClosed),
	),
	mutationVariant(
		"comment",
		{ changeNumber: positiveInteger("Change number") },
		Type.Object({ body: bodyText("Comment body") }, closed),
	),
	mutationVariant(
		"approve",
		{ changeNumber: positiveInteger("Change number") },
		Type.Optional(Type.Object({ body: Type.Optional(bodyText("Review body")) }, closed)),
	),
	mutationVariant(
		"merge",
		{ changeNumber: positiveInteger("Change number") },
		Type.Optional(
			Type.Object(
				{
					mergeMethod: Type.Optional(
						Type.Union([Type.Literal("merge"), Type.Literal("squash"), Type.Literal("rebase")]),
					),
				},
				closed,
			),
		),
	),
]);

const wikiFormat = Type.Union([Type.Literal("markdown"), Type.Literal("rdoc"), Type.Literal("asciidoc")]);
const wikiMutationSchema = Type.Union([
	mutationVariant(
		"create",
		{},
		Type.Object(
			{
				slug: nonEmptyString("Wiki page slug", 256),
				title: shortText("Wiki page title"),
				content: bodyText("Wiki page content"),
				format: Type.Optional(wikiFormat),
			},
			closed,
		),
	),
	mutationVariant(
		"update",
		{ wikiSlug: nonEmptyString("Existing wiki page slug", 256) },
		Type.Object(
			{
				title: Type.Optional(shortText("Wiki page title")),
				content: Type.Optional(bodyText("Wiki page content")),
				format: Type.Optional(wikiFormat),
			},
			nonEmptyClosed,
		),
	),
	mutationVariant("delete", { wikiSlug: nonEmptyString("Wiki page slug", 256) }),
]);

const variablesSchema = Type.Array(
	Type.Object({ name: nonEmptyString("Variable name", 128), value: nonEmptyString("Variable value", 4096) }, closed),
	{ minItems: 1, maxItems: 50 },
);
const pipelineMutationSchema = Type.Union([
	mutationVariant(
		"trigger",
		{},
		Type.Object({ ref: shortText("Branch or tag to run"), variables: Type.Optional(variablesSchema) }, closed),
	),
	mutationVariant("retry", { pipelineId: positiveInteger("Pipeline or workflow run id") }),
	mutationVariant("cancel", { pipelineId: positiveInteger("Pipeline or workflow run id") }),
	mutationVariant(
		"play_job",
		{ jobId: positiveInteger("Manual job id") },
		Type.Optional(Type.Object({ variables: Type.Optional(variablesSchema) }, closed)),
	),
	mutationVariant("retry_job", { jobId: positiveInteger("Job id") }),
	mutationVariant("cancel_job", { jobId: positiveInteger("Job id") }),
]);

const releaseFields = {
	name: Type.Optional(shortText("Release name")),
	body: Type.Optional(bodyText("Release notes")),
	ref: Type.Optional(shortText("Target branch or commit")),
	milestoneTitles: Type.Optional(stringList("Release milestone title")),
	draft: Type.Optional(Type.Boolean()),
	prerelease: Type.Optional(Type.Boolean()),
};
const releaseMutationSchema = Type.Union([
	mutationVariant(
		"create",
		{ tagName: nonEmptyString("Release tag", 256) },
		Type.Optional(Type.Object(releaseFields, closed)),
	),
	mutationVariant(
		"update",
		{ tagName: nonEmptyString("Existing release tag", 256) },
		Type.Object(releaseFields, nonEmptyClosed),
	),
]);
const transitionSchema = Type.Union([
	Type.Object(
		{
			transition: Type.Literal("submit_review"),
			requestId,
			input: Type.Object(
				{
					...changeUpdateFields,
					title: shortText("Pull request or merge request title"),
					sourceBranch: shortText("Source branch"),
					targetBranch: shortText("Target branch"),
					draft: Type.Optional(Type.Boolean({ description: "Create as draft" })),
				},
				closed,
			),
		},
		closed,
	),
	Type.Object(
		{
			transition: Type.Literal("prepare_merge"),
			changeId: positiveInteger("Pull request or merge request number"),
		},
		closed,
	),
	Type.Object(
		{
			transition: Type.Literal("merge"),
			requestId,
			changeId: positiveInteger("Pull request or merge request number"),
			input: Type.Optional(
				Type.Object(
					{
						mergeMethod: Type.Optional(
							Type.Union([Type.Literal("merge"), Type.Literal("squash"), Type.Literal("rebase")]),
						),
					},
					closed,
				),
			),
		},
		closed,
	),
	Type.Object({ transition: Type.Literal("prepare_release"), input: Type.Optional(releaseAuditInputSchema) }, closed),
	Type.Object(
		{
			transition: Type.Literal("release"),
			requestId,
			input: Type.Object({ ...releaseFields, tagName: nonEmptyString("Release tag", 256) }, closed),
		},
		closed,
	),
]);

interface ForgeToolSpec {
	name: ForgeToolName;
	label: string;
	description: string;
	promptSnippet: string;
	parameters: TSchema;
	contract: AgentToolContract;
}

const readRetry = {
	retryableErrors: ["rate_limited", "remote_failure"],
	maxAttempts: 2,
	rejectUnchangedRetry: true,
} as const;
const writeRetry = {
	retryableErrors: ["rate_limited"],
	maxAttempts: 1,
	rejectUnchangedRetry: true,
} as const;

function readContract(examples: AgentToolContract["examples"], maximumBytes = 16 * 1024): AgentToolContract {
	return defineAgentToolContract({
		version: 1,
		useWhen: ["Reading or inspecting the current repository development platform"],
		doNotUseWhen: ["The requested information is available from local Git without a remote API call"],
		effect: "read",
		idempotency: "inherent",
		retry: readRetry,
		outputBudget: { defaultBytes: maximumBytes, maximumBytes: MAX_FORGE_TOOL_RESULT_BYTES },
		examples,
	});
}

function writeContract(effect: "routine_write" | "sensitive_write" | "destructive"): AgentToolContract {
	return defineAgentToolContract({
		version: 1,
		useWhen: ["The user requested a mutation in the current GitHub or GitLab repository"],
		doNotUseWhen: ["Only inspection is required", "Required identifiers or mutation content are unknown"],
		effect,
		idempotency: "required",
		retry: writeRetry,
		outputBudget: { defaultBytes: 4 * 1024, maximumBytes: 16 * 1024 },
		examples: [],
	});
}

function serviceInput(toolName: ForgeToolName, input: Record<string, unknown>): Record<string, unknown> {
	const normalized = { ...input };
	const identifier =
		toolName === "forge_issue"
			? normalized.issueNumber
			: toolName === "forge_milestone"
				? normalized.milestoneNumber
				: toolName === "forge_change"
					? normalized.changeNumber
					: toolName === "forge_wiki"
						? normalized.wikiSlug
						: toolName === "forge_pipeline"
							? (normalized.pipelineId ?? normalized.jobId)
							: toolName === "forge_release"
								? normalized.tagName
								: undefined;
	delete normalized.issueNumber;
	delete normalized.milestoneNumber;
	delete normalized.changeNumber;
	delete normalized.wikiSlug;
	delete normalized.pipelineId;
	delete normalized.jobId;
	delete normalized.tagName;
	if (identifier !== undefined) normalized.id = identifier;
	return normalized;
}

const specs: ForgeToolSpec[] = [
	{
		name: "forge_context",
		label: "Forge context",
		description:
			"Resolve the GitLab or GitHub repository, instance version, identity, and negotiated read capabilities.",
		promptSnippet: "Inspect repository hosting context and capabilities",
		parameters: contextSchema,
		contract: readContract([{ description: "Resolve the current repository", input: {} }], 8 * 1024),
	},
	{
		name: "forge_query",
		label: "Forge query",
		description:
			"Read current-repository Forge resources. Choose exactly one operation: list, get, or get_many. Omit every field not defined by that operation; never send empty-string, zero, null, or empty-array placeholders.",
		promptSnippet: "Query GitLab or GitHub resources",
		parameters: querySchema,
		contract: readContract([
			{ description: "List open issues", input: { operation: "list", resource: "issue", state: "open" } },
			{ description: "Get issue 42", input: { operation: "get", resource: "issue", id: 42 } },
		]),
	},
	{
		name: "forge_audit",
		label: "Forge audit",
		description:
			"Evaluate the repository workflow policy against current local and remote state without changing either.",
		promptSnippet: "Audit development workflow policy",
		parameters: auditSchema,
		contract: readContract([{ description: "Audit current local review readiness", input: { workflow: "current" } }]),
	},
	{
		name: "forge_watch",
		label: "Forge watch",
		description: "Wait for pipeline, job, review, or change-request state with bounded, cancellable polling.",
		promptSnippet: "Wait for remote development state",
		parameters: watchSchema,
		contract: readContract(
			[{ description: "Wait for pipeline success", input: { resource: "pipeline", id: 42, until: ["success"] } }],
			4 * 1024,
		),
	},
	{
		name: "forge_issue",
		label: "Forge issue",
		description:
			"Create or update issues, comments, assignments, labels, and milestone associations subject to repository policy.",
		promptSnippet: "Manage issues on GitLab or GitHub",
		parameters: issueMutationSchema,
		contract: writeContract("sensitive_write"),
	},
	{
		name: "forge_milestone",
		label: "Forge milestone",
		description: "Create, update, close, or reopen milestones subject to repository policy.",
		promptSnippet: "Manage development milestones",
		parameters: milestoneMutationSchema,
		contract: writeContract("sensitive_write"),
	},
	{
		name: "forge_change",
		label: "Forge change",
		description:
			"Manage merge requests or pull requests, reviews, discussions, approvals, and merges subject to policy and approval gates.",
		promptSnippet: "Manage merge requests or pull requests",
		parameters: changeMutationSchema,
		contract: writeContract("destructive"),
	},
	{
		name: "forge_wiki",
		label: "Forge wiki",
		description: "Create, update, or delete supported project wiki content subject to policy and capability checks.",
		promptSnippet: "Manage project wiki content",
		parameters: wikiMutationSchema,
		contract: writeContract("destructive"),
	},
	{
		name: "forge_pipeline",
		label: "Forge pipeline",
		description: "Trigger, retry, cancel, or run supported pipelines and jobs subject to policy and approval gates.",
		promptSnippet: "Manage CI pipelines and jobs",
		parameters: pipelineMutationSchema,
		contract: writeContract("destructive"),
	},
	{
		name: "forge_release",
		label: "Forge release",
		description: "Create or update releases and related tags subject to policy and approval gates.",
		promptSnippet: "Manage project releases",
		parameters: releaseMutationSchema,
		contract: writeContract("sensitive_write"),
	},
	{
		name: "forge_transition",
		label: "Forge workflow",
		description: "Run a governed workflow transition such as submitting review, merging, or releasing.",
		promptSnippet: "Run a governed development workflow transition",
		parameters: transitionSchema,
		contract: writeContract("destructive"),
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
				promptGuidelines: [
					"Omit unused optional Forge arguments; never send empty strings, zero, null, or empty arrays as placeholders.",
					"After a Forge validation or permission error, change the arguments or stop; never repeat an unchanged failed call.",
				],
				parameters: spec.parameters,
				contract: spec.contract,
				executionMode: FORGE_WRITE_TOOL_NAMES.includes(spec.name as ForgeWriteToolName) ? "sequential" : "parallel",
				async execute(toolCallId, input, signal, _onUpdate, ctx): Promise<AgentToolResult<unknown>> {
					const publicInput = input as Record<string, unknown>;
					const normalizedInput = serviceInput(spec.name, publicInput);
					const extensionContext = ctx as typeof ctx | undefined;
					try {
						const result = await service.execute(spec.name, normalizedInput, signal, {
							cwd: options.cwd,
							agentDir: options.agentDir,
							toolCallId,
							interactive: extensionContext?.hasUI ?? false,
							projectTrusted: extensionContext?.isProjectTrusted() ?? false,
							confirm: (title, message) =>
								extensionContext?.uiV2?.dialogs.confirm({ title, message }) ?? Promise.resolve(false),
						});
						const action =
							typeof normalizedInput.action === "string"
								? normalizedInput.action
								: typeof normalizedInput.transition === "string"
									? normalizedInput.transition
									: undefined;
						const mutationResult =
							action && !action.startsWith("prepare_")
								? typeof result === "object" && result !== null && (result as { ok?: unknown }).ok === true
									? result
									: projectForgeMutation(
											spec.name === "forge_pipeline" && action.includes("job")
												? "job"
												: spec.name.replace(/^forge_/, ""),
											action,
											result,
											normalizedInput.id,
										)
								: result;
						const bounded = boundedForgeToolResult(mutationResult);
						return { content: [{ type: "text", text: bounded.text }], details: bounded.value };
					} catch (error) {
						if (!(error instanceof ForgeError)) throw error;
						const retryable = error.code === "rate_limited" || error.code === "remote_failure";
						throw new Error(
							JSON.stringify({
								ok: false,
								error: {
									code: error.code,
									retryable,
									message: error.message,
									details: error.details,
								},
							}),
						);
					}
				},
			};
			return [spec.name, definition];
		}),
	) as Record<ForgeToolName, ToolDefinition>;
}

export function createForgeTools(options: CreateForgeToolDefinitionsOptions) {
	return wrapToolDefinitions(Object.values(createForgeToolDefinitions(options)));
}
