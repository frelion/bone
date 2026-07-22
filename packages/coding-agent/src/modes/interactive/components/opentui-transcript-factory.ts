import type { AgentMessage } from "@frelion/bone-agent-core";
import type { AssistantMessage, ToolResultMessage } from "@frelion/bone-ai";
import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import { type AgentSessionEvent, parseSkillBlock } from "../../../core/agent-session.ts";
import type { CustomEntryViewRenderer, CustomMessageViewRenderer } from "../../../core/extensions/types.ts";
import type { ExtensionUIToolViewRenderer, ExtensionUIToolViewState } from "../../../core/extensions/ui-v2.ts";
import {
	createBranchSummaryMessage,
	createCompactionSummaryMessage,
	createCustomMessage,
} from "../../../core/messages.ts";
import type { SessionEntry } from "../../../core/session-manager.ts";
import { decodeOpenTUIImages, OpenTUIImageAttachments } from "./opentui-image.ts";
import {
	hasVisibleOpenTUIAssistantContent,
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
	view: BoneView;
}

export type OpenTUITranscriptMutation =
	| { type: "append"; item: OpenTUITranscriptItem }
	| { type: "updated"; key: string; view: BoneView }
	| { type: "ignored" };

class OpenTUIGroupedView implements BoneView {
	private readonly views: readonly BoneView[];

	constructor(views: readonly BoneView[]) {
		this.views = views;
	}

	mount(context: BoneRenderContext): BoneNode {
		const root = context.createBox({ flexDirection: "column" });
		for (const view of this.views) root.append(view.mount(context));
		return root;
	}
}

class OpenTUISafeView implements BoneView {
	private readonly primary: BoneView;
	private readonly fallback: BoneView;
	private readonly onError: ((error: unknown, surface: string) => void) | undefined;
	private readonly surface: string;

	constructor(
		primary: BoneView,
		fallback: BoneView,
		onError: ((error: unknown, surface: string) => void) | undefined,
		surface: string,
	) {
		this.primary = primary;
		this.fallback = fallback;
		this.onError = onError;
		this.surface = surface;
	}

	mount(context: BoneRenderContext): BoneNode {
		try {
			return this.primary.mount(context);
		} catch (error) {
			this.onError?.(error, this.surface);
			return this.fallback.mount(context);
		}
	}
}

class OpenTUICustomEntryFallback implements BoneView {
	mount(context: BoneRenderContext): BoneNode {
		return context.createText({ content: "[custom entry unavailable]" });
	}
}

class OpenTUIStructuredToolExecution implements BoneView {
	private readonly toolCallId: string;
	private readonly cwd: string;
	private readonly getRenderer: () => ExtensionUIToolViewRenderer | undefined;
	private readonly onError: ((error: unknown, surface: string) => void) | undefined;
	private readonly fallback: OpenTUIToolExecution;
	private readonly state: Record<string, unknown> = {};
	private args: unknown;
	private result: ToolResultMessage | undefined;
	private isPartial = true;
	private expanded = false;
	private executionStarted = false;
	private argsComplete = false;
	private context: BoneRenderContext | undefined;
	private root: BoneContainerNode | undefined;
	private currentView: BoneView | undefined;
	private currentNode: BoneNode | undefined;

	constructor(
		toolName: string,
		toolCallId: string,
		args: unknown,
		cwd: string,
		getRenderer: () => ExtensionUIToolViewRenderer | undefined,
		onError?: (error: unknown, surface: string) => void,
	) {
		this.toolCallId = toolCallId;
		this.args = args;
		this.cwd = cwd;
		this.getRenderer = getRenderer;
		this.onError = onError;
		this.fallback = new OpenTUIToolExecution(toolName, toolCallId, args);
	}

	mount(context: BoneRenderContext): BoneNode {
		this.context = context;
		this.root = context.createBox({ flexDirection: "column" });
		this.currentView = undefined;
		this.currentNode = undefined;
		this.refresh();
		return this.root;
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

	private refresh(): void {
		if (!this.context || !this.root || this.root.destroyed) return;
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
		let nextView: BoneView | undefined;
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
		let resolvedView = nextView ?? this.fallback;
		if (resolvedView === this.currentView) {
			this.currentNode?.requestRender();
			return;
		}
		let nextNode: BoneNode;
		try {
			nextNode = resolvedView.mount(this.context);
		} catch (error) {
			this.onError?.(error, "tool renderer view");
			resolvedView = this.fallback;
			nextNode = this.fallback.mount(this.context);
		}
		if (this.currentNode && !this.currentNode.destroyed) this.currentNode.destroy();
		this.currentView = resolvedView;
		this.currentNode = nextNode;
		this.root.append(this.currentNode);
	}
}

function textFromContent(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter((part): part is { type: "text"; text: string } => part?.type === "text" && typeof part.text === "string")
		.map((part) => part.text)
		.join("\n");
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
	private readonly options: Required<OpenTUITranscriptFactoryOptions>;
	private resolvers: OpenTUITranscriptFactoryResolvers;
	private streamingAssistant: { key: string; view: OpenTUIAssistantMessage } | undefined;
	private readonly pendingTools = new Map<string, OpenTUIStructuredToolExecution>();
	private readonly completedLiveTools = new Set<string>();
	private readonly toolArgs = new Map<string, unknown>();
	private readonly toolGroups = new Set<OpenTUIWorkingGroup>();
	private readonly toolGroupByCall = new Map<string, { key: string; view: OpenTUIWorkingGroup }>();
	private activeToolGroup: { key: string; view: OpenTUIWorkingGroup } | undefined;
	private toolGroupSequence = 0;
	private expandAllToolDetails = false;

	constructor(options: OpenTUITranscriptFactoryOptions = {}, resolvers: OpenTUITranscriptFactoryResolvers = {}) {
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
		for (const group of this.toolGroups) group.setExpanded(expanded);
	}

	async createSessionEntries(entries: readonly SessionEntry[]): Promise<OpenTUITranscriptItem[]> {
		const items: OpenTUITranscriptItem[] = [];
		let pendingSequence: { key: string; startedAt: number } | undefined;
		let replayGroup: { key: string; view: OpenTUIWorkingGroup; clock: { completedAt: number } } | undefined;
		const flushGroup = (): void => {
			if (!replayGroup) return;
			this.toolGroups.add(replayGroup.view);
			if (this.expandAllToolDetails) replayGroup.view.setExpanded(true);
			items.push({ key: replayGroup.key, view: replayGroup.view });
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
						view: new OpenTUIWorkingGroup(startedAt, () => clock.completedAt),
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
					view: new OpenTUICompactionSummary(
						createCompactionSummaryMessage(entry.summary, entry.tokensBefore, entry.timestamp),
					),
				};
			case "branch_summary":
				return {
					key: entry.id,
					view: new OpenTUIBranchSummary(createBranchSummaryMessage(entry.summary, entry.fromId, entry.timestamp)),
				};
			case "custom_message":
				if (!entry.display) return undefined;
				return this.createMessage(
					createCustomMessage(entry.customType, entry.content, entry.display, entry.details, entry.timestamp),
					entry.id,
				);
			case "plan_proposal":
				return { key: entry.id, view: new OpenTUIPlanProposal(entry.proposal) };
			case "custom": {
				let customView: BoneView | undefined;
				try {
					customView = this.resolvers.getEntryView?.(entry.customType)?.(entry, { expanded: false });
				} catch (error) {
					this.resolvers.onError?.(error, "custom entry renderer");
					customView = undefined;
				}
				return customView
					? {
							key: entry.id,
							view: new OpenTUISafeView(
								customView,
								new OpenTUICustomEntryFallback(),
								this.resolvers.onError,
								"custom entry view",
							),
						}
					: undefined;
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
					? new OpenTUISkillInvocation(skill)
					: new OpenTUIUserMessage(textFromContent(message.content) || "[image attachment]");
				return { key, view: await this.withImages(base, message.content) };
			}
			case "assistant":
				this.rememberToolArgs(message);
				return { key, view: this.createAssistant(message) };
			case "toolResult": {
				const view = await this.createCompletedToolView(message);
				return { key, view };
			}
			case "bashExecution": {
				const view = new OpenTUIBashExecution(message.command, message.excludeFromContext);
				view.updateFromMessage(message);
				return { key, view };
			}
			case "custom":
				if (!message.display) return undefined;
				{
					const fallback = await this.withImages(new OpenTUICustomMessage(message), message.content);
					let customView: BoneView | undefined;
					try {
						customView = this.resolvers.getMessageView?.(message.customType)?.(message, { expanded: false });
					} catch (error) {
						this.resolvers.onError?.(error, "custom message renderer");
						customView = undefined;
					}
					if (customView) {
						return {
							key,
							view: new OpenTUISafeView(customView, fallback, this.resolvers.onError, "custom message view"),
						};
					}
					return { key, view: fallback };
				}
			case "branchSummary":
				return { key, view: new OpenTUIBranchSummary(message) };
			case "compactionSummary":
				return { key, view: new OpenTUICompactionSummary(message) };
			default: {
				const exhaustive: never = message;
				return exhaustive;
			}
		}
	}

	async handleEvent(event: AgentSessionEvent): Promise<OpenTUITranscriptMutation> {
		switch (event.type) {
			case "message_start":
				if (event.message.role === "toolResult" && this.completedLiveTools.delete(event.message.toolCallId)) {
					return { type: "ignored" };
				}
				if (event.message.role === "assistant") {
					if (this.hasVisibleAssistantContent(event.message)) this.activeToolGroup = undefined;
					this.rememberToolArgs(event.message);
					const key = keyForMessage(event.message);
					const view = this.createAssistant(event.message);
					this.streamingAssistant = { key, view };
					return { type: "append", item: { key, view } };
				}
				this.activeToolGroup = undefined;
				return this.appendMessage(await this.createMessage(event.message));
			case "message_update":
				if (event.message.role !== "assistant" || !this.streamingAssistant) return { type: "ignored" };
				if (this.hasVisibleAssistantContent(event.message)) this.activeToolGroup = undefined;
				this.rememberToolArgs(event.message);
				this.streamingAssistant.view.updateContent(event.message);
				return {
					type: "updated",
					key: this.streamingAssistant.key,
					view: this.streamingAssistant.view,
				};
			case "message_end":
				if (event.message.role === "assistant" && this.streamingAssistant) {
					if (this.hasVisibleAssistantContent(event.message)) this.activeToolGroup = undefined;
					this.rememberToolArgs(event.message);
					this.streamingAssistant.view.updateContent(event.message);
					if (event.message.stopReason === "aborted" || event.message.stopReason === "error") {
						this.failPendingTools(event.message.errorMessage || "Operation aborted");
					}
					const updated = {
						type: "updated" as const,
						key: this.streamingAssistant.key,
						view: this.streamingAssistant.view,
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
				const existing = this.pendingTools.get(event.toolCallId);
				if (existing) {
					existing.updateArgs(event.args);
					existing.markExecutionStarted();
					const group = this.toolGroupByCall.get(event.toolCallId);
					return group ? { type: "updated", key: group.key, view: group.view } : { type: "ignored" };
				}
				this.toolArgs.set(event.toolCallId, event.args);
				const view = this.createToolView(event.toolName, event.toolCallId, event.args);
				view.setArgsComplete();
				view.markExecutionStarted();
				this.pendingTools.set(event.toolCallId, view);
				const isNewGroup = !this.activeToolGroup;
				if (!this.activeToolGroup) {
					const group = new OpenTUIWorkingGroup(this.options.now(), this.options.now);
					this.activeToolGroup = { key: `working-group:${++this.toolGroupSequence}`, view: group };
					this.toolGroups.add(group);
				}
				this.activeToolGroup.view.addTool(event.toolCallId, view);
				if (this.expandAllToolDetails) this.activeToolGroup.view.setExpanded(true);
				this.toolGroupByCall.set(event.toolCallId, this.activeToolGroup);
				return isNewGroup
					? { type: "append", item: { key: this.activeToolGroup.key, view: this.activeToolGroup.view } }
					: { type: "updated", key: this.activeToolGroup.key, view: this.activeToolGroup.view };
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
				if (group && this.expandAllToolDetails) group.setExpanded(true);
				if (wasPending) this.completedLiveTools.add(event.toolCallId);
				return updated;
			}
			case "agent_end":
				this.failPendingTools("Tool execution ended before producing a result");
				this.streamingAssistant = undefined;
				this.pendingTools.clear();
				this.completedLiveTools.clear();
				this.activeToolGroup = undefined;
				return { type: "ignored" };
			default:
				return { type: "ignored" };
		}
	}

	private createAssistant(message: AssistantMessage): OpenTUIAssistantMessage {
		return new OpenTUIAssistantMessage(message, {
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
		const message = toolResultFromEvent(toolName, toolCallId, result, isError);
		view.updateResult(message, partial, await this.decodeImages(message.content));
		const group = this.toolGroupByCall.get(toolCallId);
		return group ? { type: "updated", key: group.key, view: group.view } : { type: "ignored" };
	}

	private async decodeImages(content: unknown): Promise<OpenTUIImageAttachment[]> {
		if (!this.options.showImages || !Array.isArray(content)) return [];
		return decodeOpenTUIImages(content, { terminalWidth: this.options.imageWidthCells });
	}

	private async withImages(base: BoneView, content: unknown): Promise<BoneView> {
		const images = await this.decodeImages(content);
		return images.length > 0 ? new OpenTUIGroupedView([base, new OpenTUIImageAttachments(images)]) : base;
	}
}
