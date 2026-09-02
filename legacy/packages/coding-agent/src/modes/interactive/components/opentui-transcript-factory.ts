import type { AgentMessage } from "@frelion/bone-agent-core";
import { type AssistantMessage, getTextContentPhase, type ToolResultMessage } from "@frelion/bone-ai";
import { BoxRenderable, type CliRenderer, type Renderable, TextRenderable } from "@opentui/core";
import { isAgentProtocolToolResult } from "../../../core/agent-protocol.ts";
import { type AgentSessionEvent, parseSkillBlock } from "../../../core/agent-session.ts";
import type { ActionItem, ExchangeProjection } from "../../../core/exchange/index.ts";
import type { CustomEntryViewRenderer, CustomMessageViewRenderer } from "../../../core/extensions/types.ts";
import type {
	ExtensionUIToolViewRenderer,
	ExtensionUIToolViewState,
	ExtensionUIView,
} from "../../../core/extensions/ui-v2.ts";
import {
	createBranchSummaryMessage,
	createCompactionSummaryMessage,
	createCustomMessage,
} from "../../../core/messages.ts";
import type { SessionEntry } from "../../../core/session-manager.ts";
import type { SubagentProjection } from "../../../core/subagents/index.ts";
import { decodeOpenTUIImages, OpenTUIImageAttachments } from "./opentui-image.ts";
import {
	hasVisibleOpenTUIAssistantContent,
	OpenTUIAssistantMessage,
	OpenTUIPlanProposal,
	OpenTUIUserMessage,
} from "./opentui-messages.ts";
import {
	OpenTUIActionExecution,
	OpenTUIBashExecution,
	OpenTUIBranchSummary,
	OpenTUICompactionSummary,
	OpenTUICustomMessage,
	type OpenTUIImageAttachment,
	OpenTUISkillInvocation,
	type OpenTUIToolActivityKind,
	type OpenTUIToolDetailLevel,
	OpenTUIToolExecution,
	OpenTUIWorkingGroup,
} from "./opentui-rich-messages.ts";

export interface OpenTUITranscriptFactoryOptions {
	hideThinkingBlock?: boolean;
	hiddenThinkingLabel?: string;
	hideProposedPlan?: boolean;
	showImages?: boolean;
	imageWidthCells?: number;
	now?: () => number;
}

export interface OpenTUITranscriptFactoryResolvers {
	cwd?: string;
	getToolRenderer?: (toolName: string) => ExtensionUIToolViewRenderer | undefined;
	getMessageView?: (customType: string) => CustomMessageViewRenderer | undefined;
	getEntryView?: (customType: string) => CustomEntryViewRenderer | undefined;
	onError?: (error: unknown, surface: string) => void;
	onToolDetailChange?: (anchor: Renderable, mutate: () => void) => void;
}

export interface OpenTUITranscriptReplayOptions {
	activeActionId?: string;
}

export interface OpenTUITranscriptItem {
	key: string;
	root: Renderable;
}

export type OpenTUITranscriptMutation =
	| { type: "append"; item: OpenTUITranscriptItem }
	| { type: "updated"; key: string; root: Renderable }
	| { type: "ignored" };

type OpenTUIAgentSessionEvent = AgentSessionEvent;

interface PendingToolView {
	view: OpenTUIStructuredToolExecution;
	group: { key: string; view: OpenTUIWorkingGroup };
}

class OpenTUIGroupedView {
	readonly root: BoxRenderable;

	constructor(renderer: CliRenderer, views: readonly Renderable[]) {
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		for (const view of views) this.root.add(view);
	}
}

class OpenTUIStructuredToolExecution {
	readonly root: BoxRenderable;
	private readonly renderer: CliRenderer;
	private readonly toolCallId: string;
	private readonly cwd: string;
	private readonly getRenderer: () => ExtensionUIToolViewRenderer | undefined;
	private readonly getOnToolDetailChange: () => OpenTUITranscriptFactoryResolvers["onToolDetailChange"];
	private readonly onError: ((error: unknown, surface: string) => void) | undefined;
	private readonly fallback: OpenTUIToolExecution;
	private readonly structuredDetails: BoxRenderable;
	private readonly structuredContent: BoxRenderable;
	private readonly state: Record<string, unknown> = {};
	private args: unknown;
	private result: ToolResultMessage | undefined;
	private isPartial = true;
	private detailLevel: OpenTUIToolDetailLevel = "collapsed";
	private executionStarted = false;
	private argsComplete = false;
	private customView: Renderable | undefined;

	constructor(
		renderer: CliRenderer,
		toolName: string,
		toolCallId: string,
		args: unknown,
		cwd: string,
		getRenderer: () => ExtensionUIToolViewRenderer | undefined,
		getOnToolDetailChange: () => OpenTUITranscriptFactoryResolvers["onToolDetailChange"],
		onError?: (error: unknown, surface: string) => void,
	) {
		this.renderer = renderer;
		this.toolCallId = toolCallId;
		this.args = args;
		this.cwd = cwd;
		this.getRenderer = getRenderer;
		this.getOnToolDetailChange = getOnToolDetailChange;
		this.onError = onError;
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		this.fallback = new OpenTUIToolExecution(renderer, toolName, toolCallId, args, {
			onDetailLevelChange: (_level, anchor) =>
				this.requestDetailLevel(this.detailLevel === "collapsed" ? "full" : "collapsed", anchor),
			summarize: (input) => {
				try {
					return this.getRenderer()?.summarize?.(input);
				} catch (error) {
					this.onError?.(error, "tool summary renderer");
					return undefined;
				}
			},
		});
		this.structuredDetails = new BoxRenderable(renderer, {
			flexDirection: "column",
			paddingLeft: 3,
			visible: false,
		});
		this.structuredContent = new BoxRenderable(renderer, { flexDirection: "column", minWidth: 0 });
		this.structuredDetails.add(this.structuredContent);
		this.root.add(this.fallback.root);
		this.root.add(this.structuredDetails);
	}

	updateArgs(args: unknown): void {
		this.args = args;
		this.fallback.updateArgs(args);
		this.refresh();
	}

	markExecutionStarted(): void {
		this.executionStarted = true;
		this.fallback.markExecutionStarted();
		this.refresh();
	}

	setArgsComplete(): void {
		this.argsComplete = true;
		this.fallback.setArgsComplete();
		this.refresh();
	}

	updateResult(
		result: ToolResultMessage,
		partial: boolean,
		attachments: readonly OpenTUIImageAttachment[] = [],
	): void {
		this.result = result;
		this.isPartial = partial;
		this.fallback.updateResult(result, partial, attachments);
		this.refresh();
	}

	setExpanded(expanded: boolean): void {
		this.setDetailLevel(expanded ? "full" : "collapsed");
	}

	setDetailLevel(level: OpenTUIToolDetailLevel): void {
		if (this.detailLevel === level) return;
		this.detailLevel = level;
		this.refresh();
	}

	getActivityKind(): OpenTUIToolActivityKind {
		return this.fallback.getActivityKind();
	}

	getSummaryText(): string {
		return this.fallback.getSummaryText();
	}

	getSummaryNode(): Renderable {
		return this.fallback.getSummaryNode();
	}

	private refresh(): void {
		if (this.root.isDestroyed) return;
		const renderer = this.getRenderer();
		if (!renderer || this.detailLevel === "collapsed") {
			this.fallback.setDetailLevel(this.detailLevel);
			this.structuredDetails.visible = false;
			return;
		}
		this.fallback.setDetailLevel("collapsed");
		this.structuredDetails.visible = true;
		const expanded = this.detailLevel === "full";
		const renderContext: ExtensionUIToolViewState = {
			toolCallId: this.toolCallId,
			args: this.args,
			state: this.state,
			cwd: this.cwd,
			executionStarted: this.executionStarted,
			argsComplete: this.argsComplete,
			isPartial: this.isPartial,
			expanded,
			isError: this.result?.isError ?? false,
			previousView: this.customView,
		};
		let nextView: ExtensionUIView | undefined;
		try {
			nextView = this.result
				? renderer.renderResult?.(
						{
							result: {
								content: this.result.content,
								details: this.result.details,
								addedToolNames: this.result.addedToolNames,
							},
							isPartial: this.isPartial,
							expanded,
						},
						renderContext,
					)
				: renderer.renderCall?.(this.args, renderContext);
		} catch (error) {
			this.onError?.(error, "tool result renderer");
			nextView = undefined;
		}
		let resolvedView: Renderable | undefined;
		if (nextView) {
			try {
				resolvedView = resolveExtensionView(nextView, this.renderer);
				if (resolvedView.isDestroyed || (resolvedView.parent && resolvedView.parent !== this.structuredContent)) {
					throw new Error("Extension tool renderer returned an attached or destroyed renderable");
				}
			} catch (error) {
				this.onError?.(error, "tool renderer view");
				resolvedView = undefined;
			}
		}
		if (!resolvedView) {
			this.fallback.setDetailLevel(this.detailLevel);
			this.structuredDetails.visible = false;
			return;
		}
		if (resolvedView === this.customView) {
			this.customView.requestRender();
			return;
		}
		if (this.customView && !this.customView.isDestroyed) {
			this.structuredContent.remove(this.customView);
			this.customView.destroyRecursively();
		}
		this.customView = resolvedView;
		this.structuredContent.add(resolvedView);
	}

	private requestDetailLevel(level: OpenTUIToolDetailLevel, anchor: Renderable): void {
		const mutate = () => this.setDetailLevel(level);
		if (this.resolversOnToolDetailChange) this.resolversOnToolDetailChange(anchor, mutate);
		else mutate();
	}

	private get resolversOnToolDetailChange(): OpenTUITranscriptFactoryResolvers["onToolDetailChange"] {
		return this.getOnToolDetailChange();
	}
}

function resolveExtensionView(view: ExtensionUIView, renderer: CliRenderer): Renderable {
	return typeof view === "function" ? view(renderer) : view;
}

function textFromContent(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter((part): part is { type: "text"; text: string } => part?.type === "text" && typeof part.text === "string")
		.map((part) => part.text)
		.join("\n");
}

function isSetActionTool(toolName: string): boolean {
	return toolName.toLowerCase().replace(/[^a-z0-9]+/g, "_") === "set_action";
}

function semanticActionFromArgs(toolCallId: string, args: unknown): { actionId: string; title: string } | undefined {
	if (typeof args !== "object" || args === null || Array.isArray(args)) return undefined;
	const record = args as Record<string, unknown>;
	const title =
		typeof record.title === "string" ? record.title : typeof record.action === "string" ? record.action : undefined;
	return title ? { actionId: toolCallId, title } : undefined;
}

function hasFinalAnswerText(message: AssistantMessage): boolean {
	return message.content.some((part) => part.type === "text" && getTextContentPhase(part) === "final_answer");
}

function hasNonCommentaryText(message: AssistantMessage): boolean {
	return message.content.some(
		(part) => part.type === "text" && getTextContentPhase(part) !== "commentary" && part.text.trim().length > 0,
	);
}

function isTerminalAssistantMessage(message: AssistantMessage): boolean {
	return (
		message.stopReason !== undefined &&
		message.stopReason !== "error" &&
		message.stopReason !== "aborted" &&
		!message.content.some((part) => part.type === "toolCall") &&
		message.content.some(
			(part) => part.type === "text" && getTextContentPhase(part) !== "commentary" && part.text.trim().length > 0,
		)
	);
}

function keyForMessage(message: AgentMessage): string {
	if (message.role === "toolResult") return `tool:${message.toolCallId}`;
	return `${message.role}:${message.timestamp}`;
}

function toolResultFromEvent(
	toolName: string,
	toolCallId: string,
	result: { content?: ToolResultMessage["content"]; details?: unknown; addedToolNames?: string[] },
	isError: boolean,
): ToolResultMessage {
	return {
		role: "toolResult",
		toolName,
		toolCallId,
		content: result.content ?? [],
		details: result.details,
		addedToolNames: result.addedToolNames,
		isError,
		timestamp: Date.now(),
	};
}

/** Owns stable views for transcript replay and the streaming agent event lifecycle. */
export class OpenTUITranscriptFactory {
	private readonly renderer: CliRenderer;
	private readonly options: Required<OpenTUITranscriptFactoryOptions>;
	private resolvers: OpenTUITranscriptFactoryResolvers;
	private streamingAssistant:
		| { key: string; view: OpenTUIAssistantMessage; visibleContentStarted: boolean }
		| undefined;
	private readonly pendingTools = new Map<string, PendingToolView>();
	private readonly actionViews = new Map<
		string,
		{ action: OpenTUIActionExecution; group: { key: string; view: OpenTUIWorkingGroup } }
	>();
	private readonly hiddenToolCalls = new Set<string>();
	private exchangeProjection: ExchangeProjection | undefined;
	private subagentProjection: SubagentProjection = { executions: [] };
	private readonly completedLiveTools = new Set<string>();
	private readonly toolArgs = new Map<string, unknown>();
	private readonly toolUpdateGeneration = new Map<string, number>();
	private readonly toolGroups = new Set<OpenTUIWorkingGroup>();
	private activeToolGroup: { key: string; view: OpenTUIWorkingGroup; appended: boolean } | undefined;
	private toolGroupSequence = 0;
	private expandAllToolDetails = false;

	constructor(
		renderer: CliRenderer,
		options: OpenTUITranscriptFactoryOptions = {},
		resolvers: OpenTUITranscriptFactoryResolvers = {},
	) {
		this.renderer = renderer;
		this.options = {
			hideThinkingBlock: options.hideThinkingBlock ?? false,
			hiddenThinkingLabel: options.hiddenThinkingLabel ?? "Thinking...",
			hideProposedPlan: options.hideProposedPlan ?? false,
			showImages: options.showImages ?? true,
			imageWidthCells: options.imageWidthCells ?? 40,
			now: options.now ?? Date.now,
		};
		this.resolvers = resolvers;
	}

	setResolvers(resolvers: OpenTUITranscriptFactoryResolvers): void {
		this.resolvers = resolvers;
	}

	setAllToolDetailsExpanded(expanded: boolean): void {
		this.expandAllToolDetails = expanded;
		for (const group of this.toolGroups) group.setToolDetailsExpanded(expanded);
	}

	/** Drop runtime bookkeeping when the factory is no longer associated with a session. */
	reset(): void {
		this.streamingAssistant = undefined;
		this.pendingTools.clear();
		this.actionViews.clear();
		this.hiddenToolCalls.clear();
		this.exchangeProjection = undefined;
		this.subagentProjection = { executions: [] };
		this.completedLiveTools.clear();
		this.toolArgs.clear();
		this.toolUpdateGeneration.clear();
		this.toolGroups.clear();
		this.activeToolGroup = undefined;
	}

	async createSessionEntries(
		entries: readonly SessionEntry[],
		options: OpenTUITranscriptReplayOptions = {},
	): Promise<OpenTUITranscriptItem[]> {
		const items: OpenTUITranscriptItem[] = [];
		const protocolToolCallIds = new Set(
			entries.flatMap((entry) =>
				entry.type === "message" && isAgentProtocolToolResult(entry.message) ? [entry.message.toolCallId] : [],
			),
		);
		let pendingSequence: { key: string; startedAt: number } | undefined;
		let currentSemanticAction: { actionId: string; title: string } | undefined;
		const actionByCall = new Map<string, { actionId: string; title: string }>();
		const hiddenCalls = new Set<string>();
		let replayGroup:
			| {
					key: string;
					view: OpenTUIWorkingGroup;
					clock: { completedAt: number };
					actions: Map<string, { view: OpenTUIActionExecution; failed: boolean }>;
			  }
			| undefined;
		const ensureGroup = (key: string, startedAt: number): NonNullable<typeof replayGroup> => {
			if (replayGroup) return replayGroup;
			const clock = { completedAt: startedAt };
			replayGroup = {
				key,
				view: new OpenTUIWorkingGroup(this.renderer, startedAt, () => clock.completedAt),
				clock,
				actions: new Map(),
			};
			return replayGroup;
		};
		const ensureAction = (
			group: NonNullable<typeof replayGroup>,
			spec: { actionId: string; title: string },
		): { view: OpenTUIActionExecution; failed: boolean } => {
			const existing = group.actions.get(spec.actionId);
			if (existing) return existing;
			const view = this.createActionView(spec.actionId, spec.title);
			const action = { view, failed: false };
			group.actions.set(spec.actionId, action);
			group.view.addTool(spec.actionId, view);
			return action;
		};
		const flushGroup = (activeActionId?: string): void => {
			if (!replayGroup) return;
			const group = { key: replayGroup.key, view: replayGroup.view };
			for (const [actionId, action] of replayGroup.actions) {
				this.actionViews.set(actionId, { action: action.view, group });
				if (actionId !== activeActionId) replayGroup.view.markToolComplete(actionId, action.failed);
			}
			const activeAction = activeActionId ? replayGroup.actions.get(activeActionId) : undefined;
			if (activeAction && activeActionId) {
				replayGroup.view.waitForAgentEnd();
				this.activeToolGroup = { ...group, appended: true };
			} else {
				replayGroup.view.finish();
			}
			this.toolGroups.add(replayGroup.view);
			if (this.expandAllToolDetails) replayGroup.view.setToolDetailsExpanded(true);
			items.push({ key: replayGroup.key, root: replayGroup.view.root });
			replayGroup = undefined;
			pendingSequence = undefined;
		};

		for (const entry of entries) {
			if (entry.type === "message" && entry.message.role === "assistant") {
				this.rememberToolArgs(entry.message);
				const toolCalls = entry.message.content.filter(
					(content): content is Extract<AssistantMessage["content"][number], { type: "toolCall" }> =>
						content.type === "toolCall" && !protocolToolCallIds.has(content.id),
				);
				const hasVisibleContent = this.hasVisibleAssistantContent(entry.message);
				if (hasVisibleContent) {
					flushGroup();
					currentSemanticAction = undefined;
					const item = await this.createSessionEntry(entry);
					if (item) items.push(item);
				}
				for (const call of toolCalls) {
					if (isSetActionTool(call.name)) {
						hiddenCalls.add(call.id);
						const action = semanticActionFromArgs(call.id, call.arguments);
						if (!action) continue;
						currentSemanticAction = action;
						pendingSequence ??= { key: `working-group:replay:${entry.id}`, startedAt: entry.message.timestamp };
						const group = ensureGroup(pendingSequence.key, pendingSequence.startedAt);
						ensureAction(group, action);
						continue;
					}
					if (currentSemanticAction) actionByCall.set(call.id, currentSemanticAction);
				}
				const hasToolCalls = toolCalls.some((call) => !isSetActionTool(call.name));
				if (hasToolCalls && !hasVisibleContent)
					pendingSequence ??= { key: `working-group:replay:${entry.id}`, startedAt: entry.message.timestamp };
				continue;
			}

			if (entry.type === "message" && entry.message.role === "toolResult") {
				if (isAgentProtocolToolResult(entry.message)) continue;
				if (hiddenCalls.has(entry.message.toolCallId) || isSetActionTool(entry.message.toolName)) continue;
				const startedAt = pendingSequence?.startedAt ?? entry.message.timestamp;
				const group = ensureGroup(pendingSequence?.key ?? `working-group:replay:${entry.id}`, startedAt);
				group.clock.completedAt = entry.message.timestamp;
				const tool = await this.createCompletedToolView(entry.message);
				const semantic = actionByCall.get(entry.message.toolCallId);
				const spec = semantic ?? {
					actionId: `tool:${entry.message.toolCallId}`,
					title: tool.getSummaryText(),
				};
				const action = ensureAction(group, spec);
				action.view.addTool(entry.message.toolCallId, tool);
				if (!semantic) action.failed ||= entry.message.isError;
				if (!semantic) action.view.setTitle(tool.getSummaryText());
				continue;
			}

			const item = await this.createSessionEntry(entry);
			if (!item) continue;
			flushGroup();
			pendingSequence = undefined;
			items.push(item);
		}
		flushGroup(options.activeActionId);
		return items;
	}

	/**
	 * Reconcile product-level Action lifecycle from the immutable Exchange projection.
	 * Agent events remain responsible only for streaming message and ToolCall view content.
	 */
	applyExchangeProjection(projection: ExchangeProjection): OpenTUITranscriptMutation {
		this.exchangeProjection = projection;
		let mutation: OpenTUITranscriptMutation = { type: "ignored" };
		for (const exchange of projection.exchanges) {
			for (const item of exchange.items) {
				if (item.type !== "action") continue;
				const rendered = this.actionViews.get(item.id);
				if (!rendered) continue;
				const status = this.actionViewStatus(item.status);
				rendered.action.setStatus(status);
				if (item.status !== "in_progress") {
					rendered.group.view.markToolComplete(item.id, item.status === "failed");
				}
				mutation = { type: "updated", key: rendered.group.key, root: rendered.group.view.root };
			}
		}
		const activeExchange = projection.exchanges.find((exchange) => exchange.id === projection.activeExchangeId);
		for (const item of activeExchange?.items ?? []) {
			if (item.type !== "action") continue;
			const projectedMutation = this.ensureLiveAction(item.id, item.label).mutation;
			if (mutation.type !== "append") mutation = projectedMutation;
			const rendered = this.actionViews.get(item.id);
			if (!rendered) continue;
			rendered.action.setStatus(this.actionViewStatus(item.status));
			if (item.status !== "in_progress") {
				rendered.group.view.markToolComplete(item.id, item.status === "failed");
			}
		}
		return mutation;
	}

	/** Reconcile retained child-agent lifecycle without adding child transcripts to model context. */
	applySubagentProjection(projection: SubagentProjection): OpenTUITranscriptMutation {
		this.subagentProjection = projection;
		let mutation: OpenTUITranscriptMutation = { type: "ignored" };
		for (const [actionId, rendered] of this.actionViews) {
			rendered.action.setSubagents(
				projection.executions.filter((execution) => execution.origin.actionId === actionId),
			);
			mutation = { type: "updated", key: rendered.group.key, root: rendered.group.view.root };
		}
		return mutation;
	}

	async createSessionEntry(entry: SessionEntry): Promise<OpenTUITranscriptItem | undefined> {
		switch (entry.type) {
			case "message": {
				const created = await this.createMessage(entry.message, entry.id);
				return created;
			}
			case "compaction":
				return {
					key: entry.id,
					root: new OpenTUICompactionSummary(
						this.renderer,
						createCompactionSummaryMessage(entry.summary, entry.tokensBefore, entry.timestamp),
					).root,
				};
			case "branch_summary":
				return {
					key: entry.id,
					root: new OpenTUIBranchSummary(
						this.renderer,
						createBranchSummaryMessage(entry.summary, entry.fromId, entry.timestamp),
					).root,
				};
			case "custom_message":
				if (!entry.display) return undefined;
				return this.createMessage(
					createCustomMessage(entry.customType, entry.content, entry.display, entry.details, entry.timestamp),
					entry.id,
				);
			case "plan_proposal":
				return { key: entry.id, root: new OpenTUIPlanProposal(this.renderer, entry.proposal).root };
			case "custom": {
				try {
					const customView = this.resolvers.getEntryView?.(entry.customType)?.(entry, { expanded: false });
					return customView ? { key: entry.id, root: resolveExtensionView(customView, this.renderer) } : undefined;
				} catch (error) {
					this.resolvers.onError?.(error, "custom entry view");
					return {
						key: entry.id,
						root: new TextRenderable(this.renderer, { content: "[custom entry unavailable]" }),
					};
				}
			}
			case "thinking_level_change":
			case "model_change":
			case "label":
			case "session_info":
			case "collaboration_mode_change":
			case "plan_decision":
			case "question_asked":
			case "question_answered":
			case "question_cancelled":
				return undefined;
			default: {
				const exhaustive: never = entry;
				return exhaustive;
			}
		}
	}

	async createMessage(
		message: AgentMessage,
		key = keyForMessage(message),
	): Promise<OpenTUITranscriptItem | undefined> {
		switch (message.role) {
			case "user": {
				const skill = typeof message.content === "string" ? parseSkillBlock(message.content) : null;
				const base = skill
					? new OpenTUISkillInvocation(this.renderer, skill)
					: new OpenTUIUserMessage(this.renderer, textFromContent(message.content) || "[image attachment]");
				return { key, root: await this.withImages(base.root, message.content) };
			}
			case "assistant":
				this.rememberToolArgs(message);
				return { key, root: this.createAssistant(message).root };
			case "toolResult": {
				if (isAgentProtocolToolResult(message)) return undefined;
				if (isSetActionTool(message.toolName)) return undefined;
				const view = await this.createCompletedToolView(message);
				return { key, root: view.root };
			}
			case "bashExecution": {
				const view = new OpenTUIBashExecution(this.renderer, message.command, message.excludeFromContext);
				view.updateFromMessage(message);
				return { key, root: view.root };
			}
			case "custom":
				if (!message.display) return undefined;
				{
					const fallback = await this.withImages(
						new OpenTUICustomMessage(this.renderer, message).root,
						message.content,
					);
					try {
						const customView = this.resolvers.getMessageView?.(message.customType)?.(message, {
							expanded: false,
						});
						if (customView) return { key, root: resolveExtensionView(customView, this.renderer) };
					} catch (error) {
						this.resolvers.onError?.(error, "custom message view");
					}
					return { key, root: fallback };
				}
			case "branchSummary":
				return { key, root: new OpenTUIBranchSummary(this.renderer, message).root };
			case "compactionSummary":
				return { key, root: new OpenTUICompactionSummary(this.renderer, message).root };
			default: {
				const exhaustive: never = message;
				return exhaustive;
			}
		}
	}

	async handleEvent(event: OpenTUIAgentSessionEvent): Promise<OpenTUITranscriptMutation> {
		switch (event.type) {
			case "agent_start": {
				if (this.activeToolGroup && !this.activeToolGroup.view.isComplete()) {
					this.activeToolGroup.view.setActivity("Working");
					return { type: "updated", key: this.activeToolGroup.key, root: this.activeToolGroup.view.root };
				}
				this.activeToolGroup = this.createLiveWorkingGroup();
				return { type: "ignored" };
			}
			case "message_start":
				if (isAgentProtocolToolResult(event.message)) return { type: "ignored" };
				if (event.message.role === "toolResult" && this.hiddenToolCalls.delete(event.message.toolCallId)) {
					return { type: "ignored" };
				}
				if (event.message.role === "toolResult" && this.completedLiveTools.delete(event.message.toolCallId)) {
					return { type: "ignored" };
				}
				if (
					event.message.role === "user" &&
					this.activeToolGroup?.appended &&
					!this.activeToolGroup.view.isComplete()
				) {
					this.activeToolGroup.view.finish();
					this.activeToolGroup = this.createLiveWorkingGroup();
				}
				if (event.message.role === "assistant") {
					const narrativeVisible = this.hasVisibleAssistantContent(event.message);
					if (narrativeVisible) this.beginVisibleAssistantContent();
					this.activeToolGroup?.view.setNarrativeVisible(hasFinalAnswerText(event.message));
					this.rememberToolArgs(event.message);
					const key = keyForMessage(event.message);
					const view = this.createAssistant(event.message, true);
					this.streamingAssistant = { key, view, visibleContentStarted: narrativeVisible };
					if (this.activeToolGroup && !this.activeToolGroup.appended) {
						this.activeToolGroup.appended = true;
						return {
							type: "append",
							item: {
								key: this.activeToolGroup.key,
								root: new OpenTUIGroupedView(this.renderer, [view.root, this.activeToolGroup.view.root]).root,
							},
						};
					}
					return { type: "append", item: { key, root: view.root } };
				}
				return this.appendMessage(await this.createMessage(event.message));
			case "message_update": {
				if (event.message.role !== "assistant" || !this.streamingAssistant) return { type: "ignored" };
				let appendWorkingAfterCommentary = false;
				if (this.hasVisibleAssistantContent(event.message) && !this.streamingAssistant.visibleContentStarted) {
					this.beginVisibleAssistantContent();
					appendWorkingAfterCommentary =
						!hasNonCommentaryText(event.message) &&
						this.activeToolGroup !== undefined &&
						!this.activeToolGroup.appended;
					this.streamingAssistant.visibleContentStarted = true;
				}
				this.activeToolGroup?.view.setNarrativeVisible(hasFinalAnswerText(event.message));
				this.rememberToolArgs(event.message);
				this.streamingAssistant.view.updateContent(event.message, true);
				if (appendWorkingAfterCommentary && this.activeToolGroup) {
					this.activeToolGroup.appended = true;
					return {
						type: "append",
						item: { key: this.activeToolGroup.key, root: this.activeToolGroup.view.root },
					};
				}
				return {
					type: "updated",
					key: this.streamingAssistant.key,
					root: this.streamingAssistant.view.root,
				};
			}
			case "message_end":
				if (event.message.role === "user") {
					this.activeToolGroup ??= this.createLiveWorkingGroup();
					if (this.activeToolGroup.appended) return { type: "ignored" };
					this.activeToolGroup.appended = true;
					return {
						type: "append",
						item: { key: this.activeToolGroup.key, root: this.activeToolGroup.view.root },
					};
				}
				if (event.message.role === "assistant" && this.streamingAssistant) {
					let appendWorkingAfterCommentary = false;
					if (this.hasVisibleAssistantContent(event.message) && !this.streamingAssistant.visibleContentStarted) {
						this.beginVisibleAssistantContent();
						appendWorkingAfterCommentary =
							!hasNonCommentaryText(event.message) &&
							this.activeToolGroup !== undefined &&
							!this.activeToolGroup.appended;
						this.streamingAssistant.visibleContentStarted = true;
					}
					this.activeToolGroup?.view.setNarrativeVisible(
						hasFinalAnswerText(event.message) || isTerminalAssistantMessage(event.message),
					);
					this.rememberToolArgs(event.message);
					this.streamingAssistant.view.updateContent(event.message, false);
					if (event.message.stopReason === "aborted" || event.message.stopReason === "error") {
						this.failPendingTools(event.message.errorMessage || "Operation aborted");
					}
					const updated = {
						type: "updated" as const,
						key: this.streamingAssistant.key,
						root: this.streamingAssistant.view.root,
					};
					this.streamingAssistant = undefined;
					if (appendWorkingAfterCommentary && this.activeToolGroup) {
						this.activeToolGroup.appended = true;
						return {
							type: "append",
							item: { key: this.activeToolGroup.key, root: this.activeToolGroup.view.root },
						};
					}
					return updated;
				}
				return { type: "ignored" };
			case "entry_appended":
				return event.entry.type === "custom"
					? this.appendMessage(await this.createSessionEntry(event.entry))
					: { type: "ignored" };
			case "tool_execution_start": {
				if (isSetActionTool(event.toolName)) {
					this.hiddenToolCalls.add(event.toolCallId);
					return { type: "ignored" };
				}
				if (this.activeToolGroup?.view.isComplete()) this.activeToolGroup = undefined;
				const existing = this.pendingTools.get(event.toolCallId);
				if (existing) {
					existing.view.updateArgs(event.args);
					existing.view.markExecutionStarted();
					return { type: "updated", key: existing.group.key, root: existing.group.view.root };
				}
				this.toolArgs.set(event.toolCallId, event.args);
				const view = this.createToolView(event.toolName, event.toolCallId, event.args);
				view.setArgsComplete();
				view.markExecutionStarted();
				const projectedAction =
					this.getProjectedActionForToolCall(event.toolCallId) ?? this.getActiveProjectedAction();
				if (!projectedAction) {
					this.toolArgs.delete(event.toolCallId);
					this.resolvers.onError?.(
						new Error(`Agent protocol invariant violated: ${event.toolName} started without an Action`),
						"live tool execution",
					);
					return { type: "ignored" };
				}
				const created = this.ensureLiveAction(projectedAction.id, projectedAction.label);
				created.action.addTool(event.toolCallId, view);
				this.pendingTools.set(event.toolCallId, { view, group: created.group });
				if (this.expandAllToolDetails) created.action.setAllDetailsExpanded(true);
				return created.mutation;
			}
			case "tool_execution_update": {
				if (this.hiddenToolCalls.has(event.toolCallId)) return { type: "ignored" };
				const pending = this.pendingTools.get(event.toolCallId);
				pending?.view.updateArgs(event.args);
				const updated = await this.updateTool(event.toolCallId, event.toolName, event.partialResult, false, true);
				return updated;
			}
			case "tool_execution_end": {
				if (this.hiddenToolCalls.delete(event.toolCallId) || isSetActionTool(event.toolName)) {
					this.completedLiveTools.add(event.toolCallId);
					return { type: "ignored" };
				}
				const pending = this.pendingTools.get(event.toolCallId);
				const updated = await this.updateTool(event.toolCallId, event.toolName, event.result, event.isError, false);
				this.pendingTools.delete(event.toolCallId);
				const group = pending?.group.view;
				if (group && this.expandAllToolDetails) group.setToolDetailsExpanded(true);
				this.toolArgs.delete(event.toolCallId);
				this.toolUpdateGeneration.delete(event.toolCallId);
				if (pending) this.completedLiveTools.add(event.toolCallId);
				return updated;
			}
			case "auto_retry_start":
				if (!this.activeToolGroup) return { type: "ignored" };
				this.activeToolGroup.view.setActivity(`Retrying · ${event.attempt}/${event.maxAttempts}`);
				if (!this.activeToolGroup.appended) {
					this.activeToolGroup.appended = true;
					return {
						type: "append",
						item: { key: this.activeToolGroup.key, root: this.activeToolGroup.view.root },
					};
				}
				return { type: "updated", key: this.activeToolGroup.key, root: this.activeToolGroup.view.root };
			case "agent_end": {
				this.failPendingTools("Tool execution ended before producing a result");
				this.streamingAssistant = undefined;
				this.pendingTools.clear();
				this.completedLiveTools.clear();
				this.toolUpdateGeneration.clear();
				const group = this.activeToolGroup;
				if (event.willRetry) {
					if (!group) return { type: "ignored" };
					group.view.setActivity("Retrying");
					if (!group.appended) {
						group.appended = true;
						return { type: "append", item: { key: group.key, root: group.view.root } };
					}
					return { type: "updated", key: group.key, root: group.view.root };
				}
				const failed = event.messages.some(
					(message) => message.role === "assistant" && message.stopReason === "error",
				);
				if (!group) return { type: "ignored" };
				if (!failed) group.view.setNarrativeVisible(true);
				group.view.finish(failed);
				this.activeToolGroup = undefined;
				if (!group.appended && failed) {
					group.appended = true;
					return { type: "append", item: { key: group.key, root: group.view.root } };
				}
				return { type: "updated", key: group.key, root: group.view.root };
			}
			default:
				return { type: "ignored" };
		}
	}

	private createLiveWorkingGroup(): { key: string; view: OpenTUIWorkingGroup; appended: boolean } {
		const group = new OpenTUIWorkingGroup(this.renderer, this.options.now(), this.options.now);
		group.waitForAgentEnd();
		this.toolGroups.add(group);
		return {
			key: `working-group:${++this.toolGroupSequence}`,
			view: group,
			appended: false,
		};
	}

	private createActionView(actionId: string, title: string): OpenTUIActionExecution {
		return new OpenTUIActionExecution(this.renderer, actionId, title, {
			now: this.options.now,
			onDetailChange: (anchor, mutate) => {
				const onDetailChange = this.resolvers.onToolDetailChange;
				if (onDetailChange) onDetailChange(anchor, mutate);
				else mutate();
			},
		});
	}

	private ensureLiveAction(
		actionId: string,
		title: string,
	): {
		action: OpenTUIActionExecution;
		group: { key: string; view: OpenTUIWorkingGroup };
		mutation: OpenTUITranscriptMutation;
	} {
		const existing = this.actionViews.get(actionId);
		if (existing) {
			return {
				action: existing.action,
				group: existing.group,
				mutation: { type: "updated", key: existing.group.key, root: existing.group.view.root },
			};
		}
		if (this.activeToolGroup?.view.isComplete()) this.activeToolGroup = undefined;
		const isNewGroup = !this.activeToolGroup;
		this.activeToolGroup ??= this.createLiveWorkingGroup();
		const action = this.createActionView(actionId, title);
		action.setSubagents(
			this.subagentProjection.executions.filter((execution) => execution.origin.actionId === actionId),
		);
		this.activeToolGroup.view.addTool(actionId, action);
		const group = { key: this.activeToolGroup.key, view: this.activeToolGroup.view };
		this.actionViews.set(actionId, { action, group });
		const shouldAppend = !this.activeToolGroup.appended;
		if (shouldAppend) this.activeToolGroup.appended = true;
		if (this.expandAllToolDetails) action.setAllDetailsExpanded(true);
		return {
			action,
			group,
			mutation:
				isNewGroup || shouldAppend
					? { type: "append", item: { key: group.key, root: group.view.root } }
					: { type: "updated", key: group.key, root: group.view.root },
		};
	}

	private beginVisibleAssistantContent(): void {
		const group = this.activeToolGroup;
		if (!group || (!group.view.hasTools() && !group.appended)) return;
		if (!group.view.hasTools()) group.view.setNarrativeVisible(true);
		group.view.finish();
		this.activeToolGroup = this.createLiveWorkingGroup();
	}

	private createAssistant(message: AssistantMessage, streaming?: boolean): OpenTUIAssistantMessage {
		return new OpenTUIAssistantMessage(this.renderer, message, {
			hideThinkingBlock: this.options.hideThinkingBlock,
			hiddenThinkingLabel: this.options.hiddenThinkingLabel,
			hideProposedPlan: this.options.hideProposedPlan,
			streaming,
		});
	}

	private hasVisibleAssistantContent(message: AssistantMessage): boolean {
		return hasVisibleOpenTUIAssistantContent(message, {
			hideThinkingBlock: this.options.hideThinkingBlock,
			hiddenThinkingLabel: this.options.hiddenThinkingLabel,
			hideProposedPlan: this.options.hideProposedPlan,
		});
	}

	private createToolView(toolName: string, toolCallId: string, args: unknown): OpenTUIStructuredToolExecution {
		return new OpenTUIStructuredToolExecution(
			this.renderer,
			toolName,
			toolCallId,
			args,
			this.resolvers.cwd ?? process.cwd(),
			() => this.resolvers.getToolRenderer?.(toolName),
			() => this.resolvers.onToolDetailChange,
			this.resolvers.onError,
		);
	}

	private async createCompletedToolView(message: ToolResultMessage): Promise<OpenTUIStructuredToolExecution> {
		const view = this.createToolView(
			message.toolName,
			message.toolCallId,
			this.toolArgs.get(message.toolCallId) ?? {},
		);
		view.setArgsComplete();
		view.markExecutionStarted();
		view.updateResult(message, false, await this.decodeImages(message.content));
		if (this.expandAllToolDetails) view.setExpanded(true);
		return view;
	}

	private rememberToolArgs(message: AssistantMessage): void {
		for (const content of message.content) {
			if (content.type === "toolCall") this.toolArgs.set(content.id, content.arguments);
		}
	}

	private appendMessage(item: OpenTUITranscriptItem | undefined): OpenTUITranscriptMutation {
		return item ? { type: "append", item } : { type: "ignored" };
	}

	private failPendingTools(error: string): void {
		for (const [toolCallId, pending] of this.pendingTools) {
			pending.view.updateResult(
				{
					role: "toolResult",
					toolCallId,
					toolName: "tool",
					content: [{ type: "text", text: error }],
					isError: true,
					timestamp: Date.now(),
				},
				false,
			);
			const action = this.getProjectedActionForToolCall(toolCallId);
			if (action) {
				this.actionViews.get(action.id)?.action.setStatus("failed");
				pending.group.view.markToolComplete(action.id, true);
			}
			this.toolUpdateGeneration.delete(toolCallId);
		}
		this.pendingTools.clear();
	}

	private async updateTool(
		toolCallId: string,
		toolName: string,
		result: { content?: ToolResultMessage["content"]; details?: unknown; addedToolNames?: string[] },
		isError: boolean,
		partial: boolean,
	): Promise<OpenTUITranscriptMutation> {
		const pending = this.pendingTools.get(toolCallId);
		if (!pending) return { type: "ignored" };
		const generation = (this.toolUpdateGeneration.get(toolCallId) ?? 0) + 1;
		this.toolUpdateGeneration.set(toolCallId, generation);
		const message = toolResultFromEvent(toolName, toolCallId, result, isError);
		const attachments = await this.decodeImages(message.content);
		if (this.toolUpdateGeneration.get(toolCallId) !== generation || this.pendingTools.get(toolCallId) !== pending) {
			return { type: "ignored" };
		}
		pending.view.updateResult(message, partial, attachments);
		return { type: "updated", key: pending.group.key, root: pending.group.view.root };
	}

	private getActiveProjectedAction(): ActionItem | undefined {
		const activeExchangeId = this.exchangeProjection?.activeExchangeId;
		if (!activeExchangeId) return undefined;
		return this.exchangeProjection?.exchanges
			.find((exchange) => exchange.id === activeExchangeId)
			?.items.find((item): item is ActionItem => item.type === "action" && item.status === "in_progress");
	}

	private getProjectedActionForToolCall(toolCallId: string): ActionItem | undefined {
		for (const exchange of this.exchangeProjection?.exchanges ?? []) {
			for (const item of exchange.items) {
				if (item.type === "action" && item.toolCalls.some((toolCall) => toolCall.id === toolCallId)) return item;
			}
		}
		return undefined;
	}

	private actionViewStatus(status: ActionItem["status"]): "running" | "completed" | "failed" | "cancelled" {
		return status === "in_progress" ? "running" : status;
	}

	private async decodeImages(content: unknown): Promise<OpenTUIImageAttachment[]> {
		if (!this.options.showImages || !Array.isArray(content)) return [];
		return decodeOpenTUIImages(content, { terminalWidth: this.options.imageWidthCells });
	}

	private async withImages(base: Renderable, content: unknown): Promise<Renderable> {
		const images = await this.decodeImages(content);
		return images.length > 0
			? new OpenTUIGroupedView(this.renderer, [base, new OpenTUIImageAttachments(this.renderer, images).root]).root
			: base;
	}
}
