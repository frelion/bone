import type { AgentMessage } from "@frelion/bone-agent-core";
import type { AssistantMessage, ToolResultMessage } from "@frelion/bone-ai";
import { BoxRenderable, type CliRenderer, type Renderable, TextRenderable } from "@opentui/core";
import { type AgentSessionEvent, parseSkillBlock } from "../../../core/agent-session.ts";
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
import { OpenTUIClickCoordinator } from "./opentui-click.ts";
import { decodeOpenTUIImages, OpenTUIImageAttachments } from "./opentui-image.ts";
import {
	hasVisibleOpenTUIAssistantContent,
	isOpenTUICommentaryText,
	OpenTUIAssistantMessage,
	OpenTUIPlanProposal,
	OpenTUIUserMessage,
} from "./opentui-messages.ts";
import {
	OpenTUIBashExecution,
	OpenTUIBranchSummary,
	OpenTUICompactionSummary,
	OpenTUICustomMessage,
	type OpenTUIImageAttachment,
	OpenTUISkillInvocation,
	type OpenTUIToolActivityKind,
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
}

export interface OpenTUITranscriptItem {
	key: string;
	root: Renderable;
}

export type OpenTUITranscriptMutation =
	| { type: "append"; item: OpenTUITranscriptItem }
	| { type: "updated"; key: string; root: Renderable }
	| { type: "ignored" };

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
	private readonly onError: ((error: unknown, surface: string) => void) | undefined;
	private readonly fallback: OpenTUIToolExecution;
	private readonly clicks = new OpenTUIClickCoordinator();
	private readonly state: Record<string, unknown> = {};
	private args: unknown;
	private result: ToolResultMessage | undefined;
	private isPartial = true;
	private expanded = false;
	private executionStarted = false;
	private argsComplete = false;
	private currentView: Renderable | undefined;

	constructor(
		renderer: CliRenderer,
		toolName: string,
		toolCallId: string,
		args: unknown,
		cwd: string,
		getRenderer: () => ExtensionUIToolViewRenderer | undefined,
		onError?: (error: unknown, surface: string) => void,
	) {
		this.renderer = renderer;
		this.toolCallId = toolCallId;
		this.args = args;
		this.cwd = cwd;
		this.getRenderer = getRenderer;
		this.onError = onError;
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		this.root.onMouse = (event) => {
			if (this.clicks.handle(event) && event.type === "down") this.renderer.clearSelection();
		};
		this.fallback = new OpenTUIToolExecution(renderer, toolName, toolCallId, args);
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
		this.expanded = expanded;
		this.fallback.setExpanded(expanded);
		this.refresh();
	}

	getActivityKind(): OpenTUIToolActivityKind {
		return this.fallback.getActivityKind();
	}

	private refresh(): void {
		if (this.root.isDestroyed) return;
		const renderer = this.getRenderer();
		const renderContext: ExtensionUIToolViewState = {
			toolCallId: this.toolCallId,
			args: this.args,
			state: this.state,
			cwd: this.cwd,
			executionStarted: this.executionStarted,
			argsComplete: this.argsComplete,
			isPartial: this.isPartial,
			expanded: this.expanded,
			isError: this.result?.isError ?? false,
			previousView: this.currentView,
		};
		let nextView: ExtensionUIView | undefined;
		try {
			nextView = this.result
				? renderer?.renderResult?.(
						{
							result: {
								content: this.result.content,
								details: this.result.details,
								addedToolNames: this.result.addedToolNames,
							},
							isPartial: this.isPartial,
							expanded: this.expanded,
						},
						renderContext,
					)
				: renderer?.renderCall?.(this.args, renderContext);
		} catch (error) {
			this.onError?.(error, "tool result renderer");
			nextView = undefined;
		}
		let resolvedView = this.fallback.root as Renderable;
		if (nextView) {
			try {
				resolvedView = resolveExtensionView(nextView, this.renderer);
				if (resolvedView.isDestroyed || (resolvedView.parent && resolvedView.parent !== this.root)) {
					throw new Error("Extension tool renderer returned an attached or destroyed renderable");
				}
			} catch (error) {
				this.onError?.(error, "tool renderer view");
				resolvedView = this.fallback.root;
			}
		}
		if (resolvedView === this.currentView) {
			this.currentView?.requestRender();
			return;
		}
		if (this.currentView && !this.currentView.isDestroyed) this.root.remove(this.currentView);
		if (this.currentView && this.currentView !== this.fallback.root && !this.currentView.isDestroyed) {
			this.currentView.destroyRecursively();
		}
		this.currentView = resolvedView;
		if (resolvedView !== this.fallback.root) {
			this.clicks.register(resolvedView, () => this.setExpanded(!this.expanded));
		}
		this.root.add(this.currentView);
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

function commentaryFromMessage(message: AssistantMessage): string | undefined {
	const commentary = message.content
		.filter((part): part is Extract<(typeof message.content)[number], { type: "text" }> => part.type === "text")
		.filter(isOpenTUICommentaryText)
		.map((part) => part.text.trim())
		.filter(Boolean)
		.join(" ");
	return commentary || undefined;
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
	private streamingAssistant: { key: string; view: OpenTUIAssistantMessage } | undefined;
	private readonly pendingTools = new Map<string, OpenTUIStructuredToolExecution>();
	private readonly completedLiveTools = new Set<string>();
	private readonly toolArgs = new Map<string, unknown>();
	private readonly toolUpdateGeneration = new Map<string, number>();
	private readonly toolGroups = new Set<OpenTUIWorkingGroup>();
	private readonly toolGroupByCall = new Map<string, { key: string; view: OpenTUIWorkingGroup }>();
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
		this.completedLiveTools.clear();
		this.toolArgs.clear();
		this.toolUpdateGeneration.clear();
		this.toolGroupByCall.clear();
		this.toolGroups.clear();
		this.activeToolGroup = undefined;
	}

	async createSessionEntries(entries: readonly SessionEntry[]): Promise<OpenTUITranscriptItem[]> {
		const items: OpenTUITranscriptItem[] = [];
		let pendingSequence: { key: string; startedAt: number } | undefined;
		let replayGroup: { key: string; view: OpenTUIWorkingGroup; clock: { completedAt: number } } | undefined;
		const flushGroup = (): void => {
			if (!replayGroup) return;
			replayGroup.view.finish();
			this.toolGroups.add(replayGroup.view);
			if (this.expandAllToolDetails) replayGroup.view.setToolDetailsExpanded(true);
			items.push({ key: replayGroup.key, root: replayGroup.view.root });
			replayGroup = undefined;
			pendingSequence = undefined;
		};

		for (const entry of entries) {
			if (entry.type === "message" && entry.message.role === "assistant") {
				this.rememberToolArgs(entry.message);
				const hasToolCalls = entry.message.content.some((content) => content.type === "toolCall");
				if (!this.hasVisibleAssistantContent(entry.message)) {
					if (hasToolCalls)
						pendingSequence ??= { key: `working-group:replay:${entry.id}`, startedAt: entry.message.timestamp };
					continue;
				}
				flushGroup();
				pendingSequence = undefined;
				const item = await this.createSessionEntry(entry);
				if (item) items.push(item);
				continue;
			}

			if (entry.type === "message" && entry.message.role === "toolResult") {
				if (!replayGroup) {
					const startedAt = pendingSequence?.startedAt ?? entry.message.timestamp;
					const clock = { completedAt: entry.message.timestamp };
					replayGroup = {
						key: pendingSequence?.key ?? `working-group:replay:${entry.id}`,
						view: new OpenTUIWorkingGroup(this.renderer, startedAt, () => clock.completedAt),
						clock,
					};
				}
				replayGroup.clock.completedAt = entry.message.timestamp;
				const view = await this.createCompletedToolView(entry.message);
				replayGroup.view.addTool(entry.message.toolCallId, view);
				replayGroup.view.markToolComplete(entry.message.toolCallId, entry.message.isError);
				continue;
			}

			const item = await this.createSessionEntry(entry);
			if (!item) continue;
			flushGroup();
			pendingSequence = undefined;
			items.push(item);
		}
		flushGroup();
		return items;
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

	async handleEvent(event: AgentSessionEvent): Promise<OpenTUITranscriptMutation> {
		switch (event.type) {
			case "agent_start": {
				if (this.activeToolGroup && !this.activeToolGroup.view.isComplete()) {
					this.activeToolGroup.view.setActivity("Working");
					return { type: "updated", key: this.activeToolGroup.key, root: this.activeToolGroup.view.root };
				}
				const group = new OpenTUIWorkingGroup(this.renderer, this.options.now(), this.options.now);
				group.waitForAgentEnd();
				this.activeToolGroup = {
					key: `working-group:${++this.toolGroupSequence}`,
					view: group,
					appended: false,
				};
				this.toolGroups.add(group);
				return { type: "ignored" };
			}
			case "message_start":
				if (event.message.role === "toolResult" && this.completedLiveTools.delete(event.message.toolCallId)) {
					return { type: "ignored" };
				}
				if (event.message.role === "assistant") {
					this.activeToolGroup?.view.setActivity(commentaryFromMessage(event.message));
					this.rememberToolArgs(event.message);
					const key = keyForMessage(event.message);
					const view = this.createAssistant(event.message);
					this.streamingAssistant = { key, view };
					if (this.activeToolGroup && !this.activeToolGroup.appended) {
						this.activeToolGroup.appended = true;
						return {
							type: "append",
							item: {
								key: this.activeToolGroup.key,
								root: new OpenTUIGroupedView(this.renderer, [this.activeToolGroup.view.root, view.root]).root,
							},
						};
					}
					return { type: "append", item: { key, root: view.root } };
				}
				return this.appendMessage(await this.createMessage(event.message));
			case "message_update":
				if (event.message.role !== "assistant" || !this.streamingAssistant) return { type: "ignored" };
				this.activeToolGroup?.view.setActivity(commentaryFromMessage(event.message));
				this.rememberToolArgs(event.message);
				this.streamingAssistant.view.updateContent(event.message);
				return {
					type: "updated",
					key: this.streamingAssistant.key,
					root: this.streamingAssistant.view.root,
				};
			case "message_end":
				if (event.message.role === "user" && this.activeToolGroup && !this.activeToolGroup.appended) {
					this.activeToolGroup.appended = true;
					return {
						type: "append",
						item: { key: this.activeToolGroup.key, root: this.activeToolGroup.view.root },
					};
				}
				if (event.message.role === "assistant" && this.streamingAssistant) {
					this.activeToolGroup?.view.setActivity(commentaryFromMessage(event.message));
					this.rememberToolArgs(event.message);
					this.streamingAssistant.view.updateContent(event.message);
					if (event.message.stopReason === "aborted" || event.message.stopReason === "error") {
						this.failPendingTools(event.message.errorMessage || "Operation aborted");
					}
					const updated = {
						type: "updated" as const,
						key: this.streamingAssistant.key,
						root: this.streamingAssistant.view.root,
					};
					this.streamingAssistant = undefined;
					return updated;
				}
				return { type: "ignored" };
			case "entry_appended":
				return event.entry.type === "custom"
					? this.appendMessage(await this.createSessionEntry(event.entry))
					: { type: "ignored" };
			case "tool_execution_start": {
				if (this.activeToolGroup?.view.isComplete()) this.activeToolGroup = undefined;
				const existing = this.pendingTools.get(event.toolCallId);
				if (existing) {
					existing.updateArgs(event.args);
					existing.markExecutionStarted();
					const group = this.toolGroupByCall.get(event.toolCallId);
					return group ? { type: "updated", key: group.key, root: group.view.root } : { type: "ignored" };
				}
				this.toolArgs.set(event.toolCallId, event.args);
				const view = this.createToolView(event.toolName, event.toolCallId, event.args);
				view.setArgsComplete();
				view.markExecutionStarted();
				this.pendingTools.set(event.toolCallId, view);
				const isNewGroup = !this.activeToolGroup;
				if (!this.activeToolGroup) {
					const group = new OpenTUIWorkingGroup(this.renderer, this.options.now(), this.options.now);
					this.activeToolGroup = {
						key: `working-group:${++this.toolGroupSequence}`,
						view: group,
						appended: true,
					};
					this.toolGroups.add(group);
				}
				this.activeToolGroup.view.addTool(event.toolCallId, view);
				if (this.expandAllToolDetails) this.activeToolGroup.view.setToolDetailsExpanded(true);
				this.toolGroupByCall.set(event.toolCallId, this.activeToolGroup);
				return isNewGroup
					? { type: "append", item: { key: this.activeToolGroup.key, root: this.activeToolGroup.view.root } }
					: { type: "updated", key: this.activeToolGroup.key, root: this.activeToolGroup.view.root };
			}
			case "tool_execution_update": {
				const view = this.pendingTools.get(event.toolCallId);
				view?.updateArgs(event.args);
				return this.updateTool(event.toolCallId, event.toolName, event.partialResult, false, true);
			}
			case "tool_execution_end": {
				const wasPending = this.pendingTools.has(event.toolCallId);
				const updated = await this.updateTool(event.toolCallId, event.toolName, event.result, event.isError, false);
				this.pendingTools.delete(event.toolCallId);
				const group = this.toolGroupByCall.get(event.toolCallId)?.view;
				group?.markToolComplete(event.toolCallId, event.isError);
				if (group && this.expandAllToolDetails) group.setToolDetailsExpanded(true);
				this.toolGroupByCall.delete(event.toolCallId);
				this.toolArgs.delete(event.toolCallId);
				this.toolUpdateGeneration.delete(event.toolCallId);
				if (wasPending) this.completedLiveTools.add(event.toolCallId);
				return updated;
			}
			case "auto_retry_start":
				if (!this.activeToolGroup) return { type: "ignored" };
				this.activeToolGroup.view.setActivity(`Retrying · ${event.attempt}/${event.maxAttempts}`);
				return { type: "updated", key: this.activeToolGroup.key, root: this.activeToolGroup.view.root };
			case "agent_end": {
				this.failPendingTools("Tool execution ended before producing a result");
				this.streamingAssistant = undefined;
				this.pendingTools.clear();
				this.completedLiveTools.clear();
				this.toolUpdateGeneration.clear();
				this.toolGroupByCall.clear();
				const group = this.activeToolGroup;
				if (!group) return { type: "ignored" };
				if (event.willRetry) {
					group.view.setActivity("Retrying");
					return { type: "updated", key: group.key, root: group.view.root };
				}
				const failed = event.messages.some(
					(message) => message.role === "assistant" && message.stopReason === "error",
				);
				group.view.finish(failed);
				this.activeToolGroup = undefined;
				return { type: "updated", key: group.key, root: group.view.root };
			}
			default:
				return { type: "ignored" };
		}
	}

	private createAssistant(message: AssistantMessage): OpenTUIAssistantMessage {
		return new OpenTUIAssistantMessage(this.renderer, message, {
			hideThinkingBlock: this.options.hideThinkingBlock,
			hiddenThinkingLabel: this.options.hiddenThinkingLabel,
			hideProposedPlan: this.options.hideProposedPlan,
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
		for (const [toolCallId, view] of this.pendingTools) {
			view.updateResult(
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
			this.toolGroupByCall.get(toolCallId)?.view.markToolComplete(toolCallId, true);
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
		const view = this.pendingTools.get(toolCallId);
		if (!view) return { type: "ignored" };
		const generation = (this.toolUpdateGeneration.get(toolCallId) ?? 0) + 1;
		this.toolUpdateGeneration.set(toolCallId, generation);
		const message = toolResultFromEvent(toolName, toolCallId, result, isError);
		const attachments = await this.decodeImages(message.content);
		if (this.toolUpdateGeneration.get(toolCallId) !== generation || this.pendingTools.get(toolCallId) !== view) {
			return { type: "ignored" };
		}
		view.updateResult(message, partial, attachments);
		const group = this.toolGroupByCall.get(toolCallId);
		return group ? { type: "updated", key: group.key, root: group.view.root } : { type: "ignored" };
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
