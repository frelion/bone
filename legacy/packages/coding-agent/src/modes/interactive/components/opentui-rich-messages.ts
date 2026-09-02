import type { TextContent, ToolResultMessage } from "@frelion/bone-ai";
import {
	BoxRenderable,
	type CliRenderer,
	DiffRenderable,
	fg,
	MarkdownRenderable,
	type Renderable,
	StyledText,
	SyntaxStyle,
	TextAttributes,
	TextRenderable,
} from "@opentui/core";
import type { ParsedSkillBlock } from "../../../core/agent-session.ts";
import type { ExtensionUIToolSummaryInput } from "../../../core/extensions/ui-v2.ts";
import type {
	BashExecutionMessage,
	BranchSummaryMessage,
	CompactionSummaryMessage,
	CustomMessage,
} from "../../../core/messages.ts";
import type { SubagentExecutionProjection } from "../../../core/subagents/index.ts";
import { stripAnsi } from "../../../utils/ansi.ts";
import { type Theme, theme } from "../theme/theme.ts";
import { OpenTUIClickCoordinator } from "./opentui-click.ts";
import { OpenTUIRgbaImage } from "./opentui-image.ts";

const PREVIEW_LINES = 20;

function rollingHighlightText(value: string, frame: number, baseColor: string, highlightColor: string): StyledText {
	const characters = Array.from(value);
	const highlight = frame % Math.max(1, characters.length + 4);
	return new StyledText(
		characters.map((character, index) =>
			fg(Math.abs(index - highlight) <= 1 ? highlightColor : baseColor)(character),
		),
	);
}

function clearChildren(root: BoxRenderable): void {
	for (const child of root.getChildren()) child.destroyRecursively();
}

export interface OpenTUIImageAttachment {
	mimeType: string;
	pixels?: Uint8Array;
	pixelWidth?: number;
	pixelHeight?: number;
	terminalWidth?: number;
	terminalHeight?: number;
	error?: string;
}

function textContent(content: ToolResultMessage["content"]): string {
	return content
		.filter((part): part is TextContent => part.type === "text")
		.map((part) => part.text)
		.join("\n")
		.trim();
}

function customContent(message: CustomMessage<unknown>): string {
	if (typeof message.content === "string") return message.content;
	return message.content
		.filter((part): part is TextContent => part.type === "text")
		.map((part) => part.text)
		.join("\n");
}

function preview(
	content: string,
	full: boolean,
	options: { limit?: number; fromEnd?: boolean } = {},
): { content: string; hiddenLines: number; hiddenBefore: boolean } {
	const lines = content.split("\n");
	const limit = options.limit ?? PREVIEW_LINES;
	if (full || lines.length <= limit) return { content, hiddenLines: 0, hiddenBefore: false };
	return {
		content: options.fromEnd ? lines.slice(-limit).join("\n") : lines.slice(0, limit).join("\n"),
		hiddenLines: lines.length - limit,
		hiddenBefore: options.fromEnd ?? false,
	};
}

function isUnifiedDiff(content: string): boolean {
	return /^(?:diff --git |--- )/m.test(content) && /^\+\+\+ /m.test(content) && /^@@ /m.test(content);
}

function appendImageAttachments(
	renderer: CliRenderer,
	body: BoxRenderable,
	attachments: readonly OpenTUIImageAttachment[],
	viewTheme: Theme,
): void {
	for (const attachment of attachments) {
		if (
			attachment.pixels &&
			attachment.pixelWidth &&
			attachment.pixelHeight &&
			attachment.terminalWidth &&
			attachment.terminalHeight
		) {
			body.add(
				new OpenTUIRgbaImage(renderer, {
					pixels: attachment.pixels,
					pixelWidth: attachment.pixelWidth,
					pixelHeight: attachment.pixelHeight,
					terminalWidth: attachment.terminalWidth,
					terminalHeight: attachment.terminalHeight,
				}),
			);
			continue;
		}
		body.add(
			new TextRenderable(renderer, {
				content: `[image: ${attachment.mimeType}; ${attachment.error ?? "unable to decode"}]`,
				fg: viewTheme.getFgColor("warning"),
				wrapMode: "word",
			}),
		);
	}
}

abstract class RebuildableView {
	readonly root: BoxRenderable;
	protected readonly renderer: CliRenderer;

	constructor(renderer: CliRenderer) {
		this.renderer = renderer;
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
	}

	protected abstract rebuild(): void;

	protected begin(backgroundColor?: string): { renderer: CliRenderer; body: BoxRenderable } | undefined {
		if (this.root.isDestroyed) return undefined;
		clearChildren(this.root);
		this.root.add(new BoxRenderable(this.renderer, { width: "100%", height: 1 }));
		const body = new BoxRenderable(this.renderer, {
			flexDirection: "column",
			paddingX: 1,
			paddingY: 1,
			backgroundColor,
		});
		this.root.add(body);
		return { renderer: this.renderer, body };
	}
}

export interface OpenTUIToolExecutionOptions {
	theme?: Theme;
	expanded?: boolean;
	onDetailLevelChange?: (level: OpenTUIToolDetailLevel, anchor: Renderable) => void;
	summarize?: (input: ExtensionUIToolSummaryInput) => string | undefined;
}

export type OpenTUIToolDetailLevel = "collapsed" | "full";

export interface OpenTUIWorkingGroupTool {
	readonly root: BoxRenderable;
	setExpanded(expanded: boolean): void;
	setDetailLevel(level: OpenTUIToolDetailLevel): void;
	getSummaryNode?(): Renderable;
	getActivityKind?(): OpenTUIToolActivityKind;
	getSummaryText?(): string;
}

export type OpenTUIActionStatus = "running" | "completed" | "failed" | "cancelled";

export interface OpenTUIActionExecutionOptions {
	theme?: Theme;
	now?: () => number;
	inferred?: boolean;
	onDetailChange?: (anchor: Renderable, mutate: () => void) => void;
}

export type OpenTUIToolActivityKind = "inspect" | "update" | "command" | "other";

function activityKindForTool(toolName: string): OpenTUIToolActivityKind {
	const normalized = toolName.toLowerCase().replace(/[^a-z0-9]+/g, "_");
	if (/^(?:read|view|open|grep|search|find|glob|list|ls)(?:_|$)/.test(normalized)) return "inspect";
	if (/^(?:edit|write|create|delete|remove|move|copy|mkdir|patch|apply_patch)(?:_|$)/.test(normalized)) {
		return "update";
	}
	if (/^(?:bash|shell|exec|execute|command|run)(?:_|$)/.test(normalized)) return "command";
	return "other";
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
	return typeof value === "object" && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: undefined;
}

function firstString(record: Record<string, unknown> | undefined, keys: readonly string[]): string | undefined {
	for (const key of keys) {
		const value = record?.[key];
		if (typeof value === "string" && value.trim()) return value;
	}
	return undefined;
}

function compactSummaryPart(value: string, limit = 72): string {
	const compact = value.replace(/\s+/g, " ").trim();
	return compact.length <= limit ? compact : `${compact.slice(0, Math.max(1, limit - 3)).trimEnd()}...`;
}

function toolSummaryTarget(toolName: string, args: unknown): string | undefined {
	const record = asRecord(args);
	const normalized = toolName.toLowerCase().replace(/[^a-z0-9]+/g, "_");
	if (/^(?:bash|shell|exec|execute|command|run)(?:_|$)/.test(normalized)) {
		return firstString(record, ["command", "cmd", "script"]);
	}
	if (/^(?:grep|search|find)(?:_|$)/.test(normalized)) {
		const query = firstString(record, ["query", "pattern", "search", "text"]);
		const path = firstString(record, ["path", "cwd", "directory"]);
		return query && path ? `"${query}" in ${path}` : query ? `"${query}"` : path;
	}
	const path = firstString(record, ["path", "filePath", "file", "target", "directory", "cwd"]);
	if (path) return path;
	return firstString(record, ["action", "query", "pattern", "name", "url", "id"]);
}

function toolSummaryFact(toolName: string, phase: string, result: ToolResultMessage | undefined): string {
	if (phase === "failed") {
		const firstErrorLine = result
			? textContent(result.content)
					.split("\n")
					.find((line) => line.trim())
			: undefined;
		return firstErrorLine ? `failed: ${compactSummaryPart(firstErrorLine, 44)}` : phase;
	}
	if (!result || phase !== "complete") return phase;
	const details = asRecord(result.details);
	if (activityKindForTool(toolName) === "command") {
		for (const key of ["exitCode", "exit_code", "code"] as const) {
			const value = details?.[key];
			if (typeof value === "number") return `exit ${value}`;
		}
	}
	if (activityKindForTool(toolName) === "inspect") {
		const lines = textContent(result.content).split("\n").length;
		if (lines > 1) return `${lines} lines`;
	}
	return phase;
}

export function summarizeOpenTUIToolCall(
	toolName: string,
	args: unknown,
	options: { phase: string; result?: ToolResultMessage },
): string {
	const target = toolSummaryTarget(toolName, args);
	const name = compactSummaryPart(toolName, 28);
	const fact = toolSummaryFact(toolName, options.phase, options.result);
	const parts = [name];
	if (target) {
		const availableTargetLength = Math.max(16, 104 - name.length - fact.length - 6);
		parts.push(compactSummaryPart(target, availableTargetLength));
	}
	parts.push(fact);
	return compactSummaryPart(parts.join(" · "), 104);
}

export class OpenTUIToolExecution extends RebuildableView {
	private readonly toolName: string;
	private readonly toolCallId: string;
	private args: unknown;
	private result: ToolResultMessage | undefined;
	private partial = true;
	private executionStarted = false;
	private argsComplete = false;
	private detailLevel: OpenTUIToolDetailLevel;
	private readonly onDetailLevelChange: ((level: OpenTUIToolDetailLevel, anchor: Renderable) => void) | undefined;
	private readonly summarize: ((input: ExtensionUIToolSummaryInput) => string | undefined) | undefined;
	private attachments: readonly OpenTUIImageAttachment[] = [];
	private viewTheme: Theme;
	private readonly body: BoxRenderable;
	private readonly titleNode: TextRenderable;
	private readonly detailsRoot: BoxRenderable;
	private readonly argsNode: TextRenderable;
	private readonly outputRoot: BoxRenderable;
	private readonly attachmentsRoot: BoxRenderable;
	private outputNode: TextRenderable | DiffRenderable | undefined;
	private summaryText = "";
	private renderedAttachments: readonly OpenTUIImageAttachment[] = [];
	private readonly clicks = new OpenTUIClickCoordinator();
	private detailProgress: number;
	private detailTarget: number;
	private detailAnimationTimer: ReturnType<typeof setInterval> | undefined;

	constructor(
		renderer: CliRenderer,
		toolName: string,
		toolCallId: string,
		args: unknown,
		options: OpenTUIToolExecutionOptions = {},
	) {
		super(renderer);
		this.toolName = toolName;
		this.toolCallId = toolCallId;
		this.args = args;
		this.detailLevel = options.expanded ? "full" : "collapsed";
		this.onDetailLevelChange = options.onDetailLevelChange;
		this.summarize = options.summarize;
		this.detailProgress = this.detailLevel === "collapsed" ? 0 : 1;
		this.detailTarget = this.detailProgress;
		this.viewTheme = options.theme ?? theme;
		this.root.onMouse = (event) => {
			if (this.clicks.handle(event) && event.type === "down") this.renderer.clearSelection();
		};
		this.body = new BoxRenderable(renderer, { flexDirection: "column", paddingX: 1 });
		this.titleNode = new TextRenderable(renderer, {
			content: "",
			attributes: TextAttributes.BOLD,
			wrapMode: "none",
			truncate: true,
			width: "100%",
			onMouseOver: () => {
				this.titleNode.attributes = TextAttributes.BOLD | TextAttributes.UNDERLINE;
			},
			onMouseOut: () => {
				this.titleNode.attributes = TextAttributes.BOLD;
			},
		});
		this.clicks.register(
			this.titleNode,
			() => {
				this.requestDetailLevel(this.detailLevel === "collapsed" ? "full" : "collapsed");
			},
			this.renderer,
		);
		this.detailsRoot = new BoxRenderable(renderer, {
			flexDirection: "column",
			paddingLeft: 2,
			visible: this.detailLevel !== "collapsed",
			opacity: this.detailProgress,
		});
		this.argsNode = new TextRenderable(renderer, { content: "", wrapMode: "word" });
		this.outputRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.attachmentsRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.body.add(this.titleNode);
		this.detailsRoot.add(this.argsNode);
		this.detailsRoot.add(this.outputRoot);
		this.detailsRoot.add(this.attachmentsRoot);
		this.body.add(this.detailsRoot);
		this.root.add(this.body);
		this.rebuild();
	}

	updateArgs(args: unknown): void {
		this.args = args;
		this.rebuild();
	}

	markExecutionStarted(): void {
		this.executionStarted = true;
		this.rebuild();
	}

	setArgsComplete(): void {
		this.argsComplete = true;
		this.rebuild();
	}

	updateResult(result: ToolResultMessage, partial = false, attachments: readonly OpenTUIImageAttachment[] = []): void {
		if (result.toolCallId !== this.toolCallId) throw new Error("Tool result does not match this tool call");
		this.result = result;
		this.partial = partial;
		this.attachments = attachments;
		this.rebuild();
	}

	setExpanded(expanded: boolean): void {
		this.setDetailLevel(expanded ? "full" : "collapsed");
	}

	setDetailLevel(level: OpenTUIToolDetailLevel): void {
		const visible = level !== "collapsed";
		if (this.detailLevel === level && this.detailTarget === (visible ? 1 : 0)) return;
		this.detailLevel = level;
		this.detailTarget = visible ? 1 : 0;
		if (visible) this.detailsRoot.visible = true;
		this.rebuild();
		this.advanceDetailAnimation();
		this.startDetailAnimation();
	}

	getActivityKind(): OpenTUIToolActivityKind {
		return activityKindForTool(this.toolName);
	}

	getSummaryNode(): Renderable {
		return this.titleNode;
	}

	getSummaryText(): string {
		return this.summaryText;
	}

	getDetailLevel(): OpenTUIToolDetailLevel {
		return this.detailLevel;
	}

	protected rebuild(): void {
		if (this.root.isDestroyed) return;
		this.body.backgroundColor = undefined;
		const phase = this.result
			? this.partial
				? "streaming"
				: this.result.isError
					? "failed"
					: "complete"
			: this.executionStarted
				? "running"
				: this.argsComplete
					? "ready"
					: "preparing";
		const customSummary = this.summarize?.({
			args: this.args,
			phase,
			result: this.result
				? { content: this.result.content, details: this.result.details, addedToolNames: this.result.addedToolNames }
				: undefined,
			isError: this.result?.isError ?? false,
		});
		this.summaryText = compactSummaryPart(
			customSummary || summarizeOpenTUIToolCall(this.toolName, this.args, { phase, result: this.result }),
			104,
		);
		this.titleNode.content = this.summaryText;
		this.titleNode.fg = this.result?.isError
			? this.viewTheme.getFgColor("error")
			: this.viewTheme.getFgColor("toolTitle");
		const serializedArgs = JSON.stringify(this.args, null, 2);
		this.argsNode.content = serializedArgs && serializedArgs !== "{}" ? serializedArgs : "";
		this.argsNode.fg = this.viewTheme.getFgColor("toolOutput");
		this.argsNode.visible = Boolean(this.argsNode.content);
		if (!this.result) {
			this.outputRoot.visible = false;
			this.attachmentsRoot.visible = false;
			this.applyDetailAnimation();
			return;
		}
		const resultContent = textContent(this.result.content);
		const diff = resultContent && isUnifiedDiff(resultContent);
		if (diff) {
			if (!(this.outputNode instanceof DiffRenderable)) {
				clearChildren(this.outputRoot);
				this.outputNode = new DiffRenderable(this.renderer, {
					diff: resultContent,
					view: "unified",
					wrapMode: "word",
					showLineNumbers: true,
					fg: this.viewTheme.getFgColor("toolOutput"),
					addedSignColor: this.viewTheme.getFgColor("toolDiffAdded"),
					removedSignColor: this.viewTheme.getFgColor("toolDiffRemoved"),
				});
				this.outputRoot.add(this.outputNode);
			} else {
				this.outputNode.diff = resultContent;
			}
		} else {
			if (!(this.outputNode instanceof TextRenderable)) {
				clearChildren(this.outputRoot);
				this.outputNode = new TextRenderable(this.renderer, { content: "", wrapMode: "word" });
				this.outputRoot.add(this.outputNode);
			}
			this.outputNode.content = resultContent;
			this.outputNode.fg = this.result.isError
				? this.viewTheme.getFgColor("error")
				: this.viewTheme.getFgColor("toolOutput");
		}
		this.outputRoot.visible = Boolean(resultContent);
		if (
			this.detailLevel === "full" &&
			(this.renderedAttachments.length !== this.attachments.length ||
				this.renderedAttachments.some((attachment, index) => attachment !== this.attachments[index]))
		) {
			clearChildren(this.attachmentsRoot);
			appendImageAttachments(this.renderer, this.attachmentsRoot, this.attachments, this.viewTheme);
			this.renderedAttachments = this.attachments;
		}
		this.attachmentsRoot.visible = this.detailLevel === "full" && this.attachments.length > 0;
		this.applyDetailAnimation();
	}

	private requestDetailLevel(level: OpenTUIToolDetailLevel): void {
		if (this.onDetailLevelChange) this.onDetailLevelChange(level, this.titleNode);
		else this.setDetailLevel(level);
	}

	private startDetailAnimation(): void {
		if (this.detailAnimationTimer || this.detailProgress === this.detailTarget) return;
		this.detailAnimationTimer = setInterval(() => this.advanceDetailAnimation(), 40);
		(this.detailAnimationTimer as { unref?: () => void }).unref?.();
	}

	private advanceDetailAnimation(): void {
		if (this.root.isDestroyed) {
			if (this.detailAnimationTimer) clearInterval(this.detailAnimationTimer);
			this.detailAnimationTimer = undefined;
			return;
		}
		const direction = this.detailTarget > this.detailProgress ? 1 : -1;
		this.detailProgress = Math.max(0, Math.min(1, this.detailProgress + direction * 0.25));
		this.applyDetailAnimation();
		this.renderer.requestRender();
		if (this.detailProgress === this.detailTarget && this.detailAnimationTimer) {
			clearInterval(this.detailAnimationTimer);
			this.detailAnimationTimer = undefined;
		}
	}

	private applyDetailAnimation(): void {
		this.detailsRoot.opacity = this.detailProgress;
		this.detailsRoot.visible = this.detailProgress > 0;
	}
}

interface OpenTUIActionToolEntry {
	id: string;
	view: OpenTUIWorkingGroupTool;
}

interface OpenTUISubagentEntry {
	execution: SubagentExecutionProjection;
	root: BoxRenderable;
	header: TextRenderable;
	details: TextRenderable;
	expanded: boolean;
}

function subagentDisplayStatus(execution: SubagentExecutionProjection): {
	label: string;
	color: "accent" | "error" | "muted" | "toolTitle" | "warning";
} {
	if (execution.status === "starting") return { label: "◌ starting", color: "muted" };
	if (execution.status === "running") return { label: "◐ running", color: "accent" };
	if (execution.status === "cancelling") return { label: "× cancelling", color: "warning" };
	if (execution.status === "closing") return { label: "— closing", color: "muted" };
	if (execution.status === "failed" || execution.lastRunStatus === "failed") {
		return { label: "! failed", color: "error" };
	}
	if (execution.handoff?.status === "partial") return { label: "△ partial", color: "warning" };
	if (execution.status === "closed") return { label: "— closed", color: "muted" };
	if (execution.lastRunStatus === "cancelled") return { label: "× cancelled", color: "warning" };
	if (execution.lastRunStatus === "completed") return { label: "✓ completed", color: "toolTitle" };
	return { label: "○ idle", color: "muted" };
}

function subagentHandoffText(execution: SubagentExecutionProjection): string {
	const sections: string[] = [];
	if (execution.yields.length > 0) {
		const visibleYields = execution.yields.slice(-10);
		if (visibleYields.length < execution.yields.length) {
			sections.push(`Messages: … ${execution.yields.length - visibleYields.length} earlier`);
		} else {
			sections.push(`Messages${execution.unreadYieldCount > 0 ? ` (${execution.unreadYieldCount} new)` : ""}:`);
		}
		for (const yielded of visibleYields) {
			sections.push(`↑ ${yielded.kind} #${yielded.sequence}: ${yielded.message}`);
			if (yielded.artifactRefs?.length) sections.push(`  Refs: ${yielded.artifactRefs.join("; ")}`);
		}
	}
	const handoff = execution.handoff;
	if (handoff) {
		sections.push(`Handoff: ${handoff.summary.trim()}`);
		const append = (label: string, values: readonly string[] | undefined) => {
			if (values?.length) sections.push(`${label}: ${values.join("; ")}`);
		};
		append("Changed", handoff.changedFiles);
		append("Validated", handoff.validations);
		append("Risks", handoff.risks);
	} else if (execution.error) {
		sections.push(`Error: ${execution.error}`);
	}
	const text = sections.filter(Boolean).join("\n");
	return text.length <= 8_000 ? text : `${text.slice(0, 7_999)}…`;
}

function subagentAggregateStatus(entries: readonly OpenTUISubagentEntry[]): string {
	if (entries.some((entry) => ["starting", "running", "cancelling", "closing"].includes(entry.execution.status))) {
		return "active";
	}
	if (entries.some((entry) => subagentDisplayStatus(entry.execution).color === "error")) return "failed";
	if (entries.some((entry) => entry.execution.handoff?.status === "partial")) return "partial";
	if (entries.some((entry) => entry.execution.lastRunStatus === "cancelled")) return "cancelled";
	if (entries.every((entry) => entry.execution.status === "closed" && !entry.execution.lastRunStatus)) return "closed";
	return "complete";
}

export class OpenTUIActionExecution {
	readonly root: BoxRenderable;
	readonly actionId: string;
	private readonly renderer: CliRenderer;
	private readonly viewTheme: Theme;
	private readonly now: () => number;
	private readonly onDetailChange: ((anchor: Renderable, mutate: () => void) => void) | undefined;
	private readonly titleNode: TextRenderable;
	private readonly subagentsRoot: BoxRenderable;
	private readonly toolsRoot: BoxRenderable;
	private readonly tools: OpenTUIActionToolEntry[] = [];
	private readonly subagents = new Map<string, OpenTUISubagentEntry>();
	private readonly clicks = new OpenTUIClickCoordinator();
	private title: string;
	private status: OpenTUIActionStatus = "running";
	private expanded = false;
	private frame = 0;
	private animationTimer: ReturnType<typeof setInterval> | undefined;

	constructor(renderer: CliRenderer, actionId: string, title: string, options: OpenTUIActionExecutionOptions = {}) {
		this.renderer = renderer;
		this.actionId = actionId;
		this.title = compactSummaryPart(title, 104);
		this.viewTheme = options.theme ?? theme;
		this.now = options.now ?? Date.now;
		this.onDetailChange = options.onDetailChange;
		this.root = new BoxRenderable(renderer, { flexDirection: "column", paddingX: 1 });
		this.root.onMouse = (event) => {
			if (this.clicks.handle(event) && event.type === "down") this.renderer.clearSelection();
		};
		this.titleNode = new TextRenderable(renderer, {
			content: "",
			attributes: TextAttributes.BOLD,
			wrapMode: "none",
			truncate: true,
			width: "100%",
			onMouseOver: () => {
				this.titleNode.attributes = TextAttributes.BOLD | TextAttributes.UNDERLINE;
			},
			onMouseOut: () => {
				this.titleNode.attributes = TextAttributes.BOLD;
			},
		});
		this.clicks.register(this.titleNode, () => this.requestExpanded(!this.expanded), renderer);
		this.toolsRoot = new BoxRenderable(renderer, { flexDirection: "column", paddingLeft: 2 });
		this.toolsRoot.visible = false;
		this.subagentsRoot = new BoxRenderable(renderer, { flexDirection: "column", paddingLeft: 2 });
		this.subagentsRoot.visible = false;
		this.root.add(this.titleNode);
		this.root.add(this.toolsRoot);
		this.root.add(this.subagentsRoot);
		this.rebuild();
	}

	addTool(id: string, view: OpenTUIWorkingGroupTool): void {
		if (this.tools.some((entry) => entry.id === id)) return;
		this.tools.push({ id, view });
		this.toolsRoot.add(view.root);
		this.toolsRoot.visible = this.expanded;
	}

	setTitle(title: string): void {
		const normalized = compactSummaryPart(title, 104);
		if (!normalized || normalized === this.title) return;
		this.title = normalized;
		this.rebuild();
	}

	setStatus(status: OpenTUIActionStatus): void {
		if (this.status === status) return;
		this.status = status;
		this.rebuild();
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.toolsRoot.visible = expanded;
		this.subagentsRoot.visible = this.status === "running" || expanded;
		if (!expanded) {
			for (const entry of this.tools) entry.view.setDetailLevel("collapsed");
			for (const entry of this.subagents.values()) this.setSubagentExpanded(entry, false);
		}
		this.renderer.requestRender();
	}

	setAllDetailsExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.toolsRoot.visible = expanded;
		this.subagentsRoot.visible = this.status === "running" || expanded;
		for (const entry of this.tools) entry.view.setDetailLevel(expanded ? "full" : "collapsed");
		for (const entry of this.subagents.values()) this.setSubagentExpanded(entry, expanded);
		this.renderer.requestRender();
	}

	setSubagents(executions: readonly SubagentExecutionProjection[]): void {
		const nextRefs = new Set(executions.map((execution) => execution.agentRef));
		for (const [agentRef, entry] of this.subagents) {
			if (nextRefs.has(agentRef)) continue;
			entry.root.destroyRecursively();
			this.subagents.delete(agentRef);
			this.clicks.reset();
		}
		for (const execution of executions) {
			const existing = this.subagents.get(execution.agentRef);
			if (existing) {
				existing.execution = execution;
				this.rebuildSubagent(existing);
				continue;
			}
			const root = new BoxRenderable(this.renderer, { flexDirection: "column" });
			const header = new TextRenderable(this.renderer, {
				content: "",
				wrapMode: "none",
				truncate: true,
				width: "100%",
			});
			const details = new TextRenderable(this.renderer, {
				content: "",
				fg: this.viewTheme.getFgColor("muted"),
				paddingLeft: 2,
				wrapMode: "word",
				visible: false,
			});
			root.add(header);
			root.add(details);
			const entry: OpenTUISubagentEntry = { execution, root, header, details, expanded: false };
			this.clicks.register(
				header,
				() => {
					if (!subagentHandoffText(entry.execution)) return;
					const mutate = () => this.setSubagentExpanded(entry, !entry.expanded);
					if (this.onDetailChange) this.onDetailChange(header, mutate);
					else mutate();
				},
				this.renderer,
			);
			this.subagents.set(execution.agentRef, entry);
			this.subagentsRoot.add(root);
			this.rebuildSubagent(entry);
		}
		this.subagentsRoot.visible = this.subagents.size > 0 && (this.status === "running" || this.expanded);
		this.rebuild();
		this.renderer.requestRender();
	}

	getSummaryNode(): Renderable {
		return this.titleNode;
	}

	getStatus(): OpenTUIActionStatus {
		return this.status;
	}

	tickActivity(): void {
		if (this.root.isDestroyed) {
			this.stopAnimation();
			this.clicks.reset();
			return;
		}
		if (this.status !== "running") return;
		this.frame = Math.floor(this.now() / 80);
		this.rebuild();
		this.renderer.requestRender();
	}

	private requestExpanded(expanded: boolean): void {
		const mutate = () => this.setExpanded(expanded);
		if (this.onDetailChange) this.onDetailChange(this.titleNode, mutate);
		else mutate();
	}

	private rebuild(): void {
		if (this.root.isDestroyed) return;
		const aggregate =
			this.status !== "running" && this.subagents.size > 0
				? ` · ${this.subagents.size} agent${this.subagents.size === 1 ? "" : "s"} · ${subagentAggregateStatus([
						...this.subagents.values(),
					])}`
				: "";
		const unreadYieldCount = [...this.subagents.values()].reduce(
			(total, entry) => total + entry.execution.unreadYieldCount,
			0,
		);
		const unreadAggregate =
			this.status !== "running" && unreadYieldCount > 0
				? ` · ${unreadYieldCount} new message${unreadYieldCount === 1 ? "" : "s"}`
				: "";
		const title = `${this.title}${aggregate}${unreadAggregate}`;
		this.subagentsRoot.visible = this.subagents.size > 0 && (this.status === "running" || this.expanded);
		if (this.status === "running") {
			this.titleNode.content = rollingHighlightText(
				title,
				this.frame,
				this.viewTheme.getFgColor("toolTitle"),
				this.viewTheme.getFgColor("accent"),
			);
			this.titleNode.fg = undefined;
			this.startAnimation();
			return;
		}
		this.stopAnimation();
		this.titleNode.content = title;
		this.titleNode.fg =
			this.status === "failed" ? this.viewTheme.getFgColor("error") : this.viewTheme.getFgColor("toolTitle");
	}

	private rebuildSubagent(entry: OpenTUISubagentEntry): void {
		const display = subagentDisplayStatus(entry.execution);
		const messageLabel =
			entry.execution.unreadYieldCount > 0
				? ` · ${entry.execution.unreadYieldCount} new message${entry.execution.unreadYieldCount === 1 ? "" : "s"}`
				: entry.execution.yields.length > 0
					? ` · ${entry.execution.yields.length} message${entry.execution.yields.length === 1 ? "" : "s"}`
					: "";
		entry.header.content = `${display.label}  ${compactSummaryPart(entry.execution.label, 72)}${messageLabel}`;
		entry.header.fg = this.viewTheme.getFgColor(display.color);
		entry.details.content = subagentHandoffText(entry.execution);
		entry.details.visible = entry.expanded && Boolean(entry.details.content);
	}

	private setSubagentExpanded(entry: OpenTUISubagentEntry, expanded: boolean): void {
		entry.expanded = expanded && Boolean(subagentHandoffText(entry.execution));
		this.rebuildSubagent(entry);
		this.renderer.requestRender();
	}

	private startAnimation(): void {
		if (this.animationTimer || this.status !== "running") return;
		this.animationTimer = setInterval(() => this.tickActivity(), 80);
		(this.animationTimer as { unref?: () => void }).unref?.();
	}

	private stopAnimation(): void {
		if (!this.animationTimer) return;
		clearInterval(this.animationTimer);
		this.animationTimer = undefined;
	}
}

interface WorkingGroupEntry {
	id: string;
	view: OpenTUIActionExecution;
	complete: boolean;
	failed: boolean;
}

export class OpenTUIWorkingGroup extends RebuildableView {
	private readonly startedAt: number;
	private readonly now: () => number;
	private readonly entries: WorkingGroupEntry[] = [];
	private readonly viewTheme: Theme;
	private readonly header: BoxRenderable;
	private readonly summaryNode: TextRenderable;
	private readonly detailsRoot: BoxRenderable;
	private completedAt: number | undefined;
	private activityMessage: string | undefined;
	private completeWithTools = true;
	private failed = false;
	private narrativeVisible = false;
	private frame = 0;
	private animationTimer: ReturnType<typeof setInterval> | undefined;

	constructor(renderer: CliRenderer, startedAt = Date.now(), now: () => number = Date.now, viewTheme: Theme = theme) {
		super(renderer);
		this.startedAt = startedAt;
		this.now = now;
		this.viewTheme = viewTheme;
		this.root.add(new BoxRenderable(renderer, { width: "100%", height: 1 }));
		this.header = new BoxRenderable(renderer, {
			flexDirection: "column",
			paddingX: 1,
		});
		this.summaryNode = new TextRenderable(renderer, { content: "" });
		this.header.add(this.summaryNode);
		this.detailsRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.root.add(this.detailsRoot);
		this.root.add(this.header);
		this.rebuild();
	}

	addTool(id: string, view: OpenTUIActionExecution): void {
		if (this.entries.some((entry) => entry.id === id)) return;
		if (this.completeWithTools && this.completedAt !== undefined) {
			this.completedAt = undefined;
		}
		this.entries.push({ id, view, complete: false, failed: false });
		this.detailsRoot.add(view.root);
		this.rebuild();
	}

	markToolComplete(id: string, failed: boolean): void {
		const entry = this.entries.find((candidate) => candidate.id === id);
		if (!entry) return;
		entry.complete = true;
		entry.failed = failed;
		if (entry.view.getStatus() === "running") entry.view.setStatus(failed ? "failed" : "completed");
		if (this.completeWithTools && this.entries.every((candidate) => candidate.complete)) this.finish(failed);
		this.rebuild();
	}

	waitForAgentEnd(): void {
		this.completeWithTools = false;
	}

	setActivity(message: string | undefined): void {
		const normalized = message?.replace(/\s+/g, " ").trim().slice(0, 140);
		this.activityMessage = normalized || undefined;
		this.rebuild();
	}

	setNarrativeVisible(visible: boolean): void {
		this.narrativeVisible = visible;
		this.rebuild();
	}

	finish(failed = false): void {
		this.failed ||= failed;
		if (this.completedAt !== undefined) {
			this.rebuild();
			return;
		}
		this.completedAt = this.now();
		this.rebuild();
	}

	setExpanded(expanded: boolean): void {
		for (const entry of this.entries) entry.view.setAllDetailsExpanded(expanded);
	}

	setToolDetailsExpanded(expanded: boolean): void {
		this.setExpanded(expanded);
	}

	isComplete(): boolean {
		return this.completedAt !== undefined;
	}

	hasTools(): boolean {
		return this.entries.length > 0;
	}

	protected rebuild(): void {
		if (this.root.isDestroyed) return;
		const failed = this.failed || this.entries.some((entry) => entry.failed);
		const count = this.entries.length;
		const hasActiveTools = this.entries.some((entry) => !entry.complete);
		this.header.visible = !this.narrativeVisible && (this.completedAt !== undefined ? count === 0 : !hasActiveTools);
		if (!this.header.visible) {
			this.stopAnimation();
			return;
		}
		const elapsedSeconds = Math.max(1, Math.round(((this.completedAt ?? this.now()) - this.startedAt) / 1000));
		const activity = this.activityMessage ?? "Working";
		if (this.completedAt !== undefined) {
			this.stopAnimation();
			this.summaryNode.content = `${failed ? "✗ Work failed" : "✓ Completed"} · ${elapsedSeconds}s`;
			this.summaryNode.fg = this.viewTheme.getFgColor(failed ? "error" : "muted");
			this.summaryNode.attributes = TextAttributes.NONE;
		} else {
			this.summaryNode.content = rollingHighlightText(
				activity,
				this.frame,
				this.viewTheme.getFgColor("toolTitle"),
				this.viewTheme.getFgColor("accent"),
			);
			this.summaryNode.attributes = TextAttributes.BOLD;
			this.startAnimation();
		}
	}

	private startAnimation(): void {
		if (this.animationTimer || this.completedAt !== undefined || !this.header.visible) return;
		this.animationTimer = setInterval(() => this.advanceAnimation(), 80);
		(this.animationTimer as { unref?: () => void }).unref?.();
	}

	private advanceAnimation(): void {
		if (this.root.isDestroyed || this.completedAt !== undefined || !this.header.visible) {
			this.stopAnimation();
			return;
		}
		this.frame++;
		this.rebuild();
		this.renderer.requestRender();
	}

	private stopAnimation(): void {
		if (!this.animationTimer) return;
		clearInterval(this.animationTimer);
		this.animationTimer = undefined;
	}
}

export class OpenTUIBashExecution extends RebuildableView {
	private output = "";
	private status: "running" | "complete" | "cancelled" | "error" = "running";
	private exitCode: number | undefined;
	private expanded = false;
	private truncated = false;
	private fullOutputPath: string | undefined;
	private readonly excluded: boolean;
	private readonly viewTheme: Theme;
	private readonly body: BoxRenderable;
	private readonly commandNode: TextRenderable;
	private readonly outputNode: TextRenderable;
	private readonly detailsNode: TextRenderable;

	constructor(renderer: CliRenderer, command: string, excludeFromContext = false, viewTheme: Theme = theme) {
		super(renderer);
		this.excluded = excludeFromContext;
		this.viewTheme = viewTheme;
		this.root.add(new BoxRenderable(renderer, { width: "100%", height: 1 }));
		this.body = new BoxRenderable(renderer, { flexDirection: "column", paddingX: 1, paddingY: 1 });
		this.commandNode = new TextRenderable(renderer, {
			content: `$ ${command}`,
			fg: viewTheme.getFgColor(this.excluded ? "dim" : "bashMode"),
			attributes: TextAttributes.BOLD,
		});
		this.outputNode = new TextRenderable(renderer, { content: "", wrapMode: "word" });
		this.detailsNode = new TextRenderable(renderer, { content: "" });
		this.body.add(this.commandNode);
		this.body.add(this.outputNode);
		this.body.add(this.detailsNode);
		this.root.add(this.body);
		this.rebuild();
	}

	appendOutput(chunk: string): void {
		this.output += stripAnsi(chunk).replace(/\r\n/g, "\n").replace(/\r/g, "\n");
		this.rebuild();
	}

	setComplete(exitCode: number | undefined, cancelled: boolean, truncated = false, fullOutputPath?: string): void {
		this.exitCode = exitCode;
		this.status = cancelled ? "cancelled" : exitCode && exitCode !== 0 ? "error" : "complete";
		this.truncated = truncated;
		this.fullOutputPath = fullOutputPath;
		this.rebuild();
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.rebuild();
	}

	updateFromMessage(message: BashExecutionMessage): void {
		this.output = stripAnsi(message.output);
		this.setComplete(message.exitCode, message.cancelled, message.truncated, message.fullOutputPath);
	}

	getOutput(): string {
		return this.output;
	}

	protected rebuild(): void {
		if (this.root.isDestroyed) return;
		const failed = this.status === "error";
		this.body.backgroundColor = failed
			? this.viewTheme.getBgColor("toolErrorBg")
			: this.viewTheme.getBgColor("customMessageBg");
		const output = preview(this.output, this.expanded);
		this.outputNode.content = output.content;
		this.outputNode.fg = this.viewTheme.getFgColor("muted");
		this.outputNode.visible = Boolean(output.content);
		const details: string[] = [];
		if (output.hiddenLines > 0) details.push(`${output.hiddenLines} earlier lines hidden`);
		if (this.status === "running") details.push("Running...");
		if (this.status === "cancelled") details.push("Cancelled");
		if (failed) details.push(`Exited with code ${this.exitCode}`);
		if (this.truncated && this.fullOutputPath) details.push(`Output truncated: ${this.fullOutputPath}`);
		this.detailsNode.content = details.join("\n");
		this.detailsNode.fg = failed ? this.viewTheme.getFgColor("error") : this.viewTheme.getFgColor("muted");
		this.detailsNode.attributes = failed ? TextAttributes.NONE : TextAttributes.DIM;
		this.detailsNode.visible = details.length > 0;
	}
}

export type OpenTUIStatusKind = "working" | "retry" | "compaction" | "branchSummary";

export class OpenTUIStatusView extends RebuildableView {
	private message: string;
	private frame = 0;
	private active = true;
	private readonly kind: OpenTUIStatusKind;
	private viewTheme: Theme;
	private animationTimer: ReturnType<typeof setInterval> | undefined;

	constructor(renderer: CliRenderer, kind: OpenTUIStatusKind, message: string, viewTheme: Theme = theme) {
		super(renderer);
		this.kind = kind;
		this.message = message;
		this.viewTheme = viewTheme;
		this.rebuild();
	}

	setMessage(message: string): void {
		this.message = message;
		this.active = message !== "Ready";
		this.rebuild();
	}

	updateTheme(nextTheme: Theme): void {
		this.viewTheme = nextTheme;
		this.rebuild();
	}

	tick(): void {
		this.frame++;
		this.rebuild();
	}

	stop(): void {
		this.active = false;
		this.rebuild();
	}

	protected rebuild(): void {
		if (this.root.isDestroyed) return;
		clearChildren(this.root);
		this.root.visible = this.message !== "Ready";
		if (!this.root.visible) {
			this.stopAnimation();
			return;
		}
		const baseColor = this.viewTheme.getFgColor(this.kind === "retry" ? "warning" : "toolTitle");
		const highlightColor = this.viewTheme.getFgColor("accent");
		this.root.add(
			new TextRenderable(this.renderer, {
				content: this.active
					? rollingHighlightText(this.message, this.frame, baseColor, highlightColor)
					: `· ${this.message}`,
				paddingX: 1,
				fg: baseColor,
			}),
		);
		if (this.active) this.startAnimation();
		else this.stopAnimation();
	}

	private startAnimation(): void {
		if (this.animationTimer || !this.active || !this.root.visible) return;
		this.animationTimer = setInterval(() => this.tick(), 80);
		(this.animationTimer as { unref?: () => void }).unref?.();
	}

	private stopAnimation(): void {
		if (!this.animationTimer) return;
		clearInterval(this.animationTimer);
		this.animationTimer = undefined;
	}
}

abstract class ExpandableSummaryView extends RebuildableView {
	protected expanded = false;
	protected readonly viewTheme: Theme;

	constructor(renderer: CliRenderer, viewTheme: Theme) {
		super(renderer);
		this.viewTheme = viewTheme;
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.rebuild();
	}

	protected renderSummary(label: string, collapsed: string, markdown: string): void {
		const mounted = this.begin(this.viewTheme.getBgColor("customMessageBg"));
		if (!mounted) return;
		mounted.body.add(
			new TextRenderable(mounted.renderer, {
				content: `[${label}]`,
				fg: this.viewTheme.getFgColor("customMessageLabel"),
				attributes: TextAttributes.BOLD,
			}),
		);
		if (this.expanded) {
			mounted.body.add(
				new MarkdownRenderable(mounted.renderer, {
					content: markdown,
					fg: this.viewTheme.getFgColor("customMessageText"),
					syntaxStyle: SyntaxStyle.fromStyles({
						default: { fg: this.viewTheme.getFgColor("customMessageText") },
					}),
				}),
			);
		} else {
			mounted.body.add(
				new TextRenderable(mounted.renderer, {
					content: collapsed,
					fg: this.viewTheme.getFgColor("customMessageText"),
				}),
			);
		}
	}
}

export class OpenTUICompactionSummary extends ExpandableSummaryView {
	private readonly message: CompactionSummaryMessage;

	constructor(renderer: CliRenderer, message: CompactionSummaryMessage, viewTheme: Theme = theme) {
		super(renderer, viewTheme);
		this.message = message;
		this.rebuild();
	}

	protected rebuild(): void {
		const tokens = this.message.tokensBefore.toLocaleString();
		this.renderSummary(
			"compaction",
			`Compacted from ${tokens} tokens`,
			`**Compacted from ${tokens} tokens**\n\n${this.message.summary}`,
		);
	}
}

export class OpenTUIBranchSummary extends ExpandableSummaryView {
	private readonly message: BranchSummaryMessage;

	constructor(renderer: CliRenderer, message: BranchSummaryMessage, viewTheme: Theme = theme) {
		super(renderer, viewTheme);
		this.message = message;
		this.rebuild();
	}

	protected rebuild(): void {
		this.renderSummary("branch", "Branch summary", `**Branch Summary**\n\n${this.message.summary}`);
	}
}

export class OpenTUISkillInvocation extends ExpandableSummaryView {
	private readonly skill: ParsedSkillBlock;

	constructor(renderer: CliRenderer, skill: ParsedSkillBlock, viewTheme: Theme = theme) {
		super(renderer, viewTheme);
		this.skill = skill;
		this.rebuild();
	}

	protected rebuild(): void {
		this.renderSummary("skill", this.skill.name, `**${this.skill.name}**\n\n${this.skill.content}`);
	}
}

export class OpenTUICustomMessage extends ExpandableSummaryView {
	private message: CustomMessage<unknown>;

	constructor(renderer: CliRenderer, message: CustomMessage<unknown>, viewTheme: Theme = theme) {
		super(renderer, viewTheme);
		this.message = message;
		this.rebuild();
	}

	updateContent(message: CustomMessage<unknown>): void {
		this.message = message;
		this.rebuild();
	}

	protected rebuild(): void {
		const content = customContent(this.message);
		const collapsed = preview(content, false);
		this.renderSummary(this.message.customType, collapsed.content || this.message.customType, content);
	}
}

export function textOnlyToolResult(
	toolName: string,
	toolCallId: string,
	text: string,
	isError = false,
): ToolResultMessage {
	const content: TextContent[] = [{ type: "text", text }];
	return { role: "toolResult", toolCallId, toolName, content, isError, timestamp: Date.now() };
}
