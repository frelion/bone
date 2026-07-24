import type { TextContent, ToolResultMessage } from "@frelion/bone-ai";
import {
	BoxRenderable,
	type CliRenderer,
	DiffRenderable,
	fg,
	MarkdownRenderable,
	StyledText,
	SyntaxStyle,
	TextAttributes,
	TextRenderable,
} from "@opentui/core";
import type { ParsedSkillBlock } from "../../../core/agent-session.ts";
import type {
	BashExecutionMessage,
	BranchSummaryMessage,
	CompactionSummaryMessage,
	CustomMessage,
} from "../../../core/messages.ts";
import { stripAnsi } from "../../../utils/ansi.ts";
import { type Theme, theme } from "../theme/theme.ts";
import { OpenTUIClickCoordinator } from "./opentui-click.ts";
import { OpenTUIRgbaImage } from "./opentui-image.ts";

const PREVIEW_LINES = 20;

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

function preview(content: string, expanded: boolean): { content: string; hiddenLines: number } {
	const lines = content.split("\n");
	if (expanded || lines.length <= PREVIEW_LINES) return { content, hiddenLines: 0 };
	return { content: lines.slice(-PREVIEW_LINES).join("\n"), hiddenLines: lines.length - PREVIEW_LINES };
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
}

export interface OpenTUIWorkingGroupTool {
	readonly root: BoxRenderable;
	setExpanded(expanded: boolean): void;
	getActivityKind?(): OpenTUIToolActivityKind;
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

export class OpenTUIToolExecution extends RebuildableView {
	private readonly toolName: string;
	private readonly toolCallId: string;
	private args: unknown;
	private result: ToolResultMessage | undefined;
	private partial = true;
	private executionStarted = false;
	private argsComplete = false;
	private expanded: boolean;
	private attachments: readonly OpenTUIImageAttachment[] = [];
	private viewTheme: Theme;
	private readonly body: BoxRenderable;
	private readonly titleNode: TextRenderable;
	private readonly detailsRoot: BoxRenderable;
	private readonly argsNode: TextRenderable;
	private readonly outputRoot: BoxRenderable;
	private readonly attachmentsRoot: BoxRenderable;
	private readonly hiddenNode: TextRenderable;
	private outputNode: TextRenderable | DiffRenderable | undefined;
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
		this.expanded = options.expanded ?? false;
		this.detailProgress = this.expanded ? 1 : 0;
		this.detailTarget = this.detailProgress;
		this.viewTheme = options.theme ?? theme;
		this.root.onMouse = (event) => {
			if (this.clicks.handle(event) && event.type === "down") this.renderer.clearSelection();
		};
		this.root.add(new BoxRenderable(renderer, { width: "100%", height: 1 }));
		this.body = new BoxRenderable(renderer, { flexDirection: "column", paddingX: 1, paddingY: 1 });
		this.titleNode = new TextRenderable(renderer, {
			content: "",
			attributes: TextAttributes.BOLD,
		});
		this.clicks.register(this.titleNode, () => this.setExpanded(!this.expanded));
		this.detailsRoot = new BoxRenderable(renderer, {
			flexDirection: "column",
			visible: this.expanded,
			opacity: this.detailProgress,
		});
		this.argsNode = new TextRenderable(renderer, { content: "", wrapMode: "word" });
		this.outputRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.attachmentsRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.hiddenNode = new TextRenderable(renderer, { content: "", attributes: TextAttributes.DIM });
		this.body.add(this.titleNode);
		this.detailsRoot.add(this.argsNode);
		this.detailsRoot.add(this.outputRoot);
		this.detailsRoot.add(this.attachmentsRoot);
		this.detailsRoot.add(this.hiddenNode);
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
		if (this.expanded === expanded && this.detailTarget === (expanded ? 1 : 0)) return;
		this.expanded = expanded;
		this.detailTarget = expanded ? 1 : 0;
		if (expanded) this.detailsRoot.visible = true;
		this.rebuild();
		this.advanceDetailAnimation();
		this.startDetailAnimation();
	}

	getActivityKind(): OpenTUIToolActivityKind {
		return activityKindForTool(this.toolName);
	}

	protected rebuild(): void {
		if (this.root.isDestroyed) return;
		const background = this.partial
			? this.viewTheme.getBgColor("toolPendingBg")
			: this.result?.isError
				? this.viewTheme.getBgColor("toolErrorBg")
				: this.viewTheme.getBgColor("toolSuccessBg");
		this.body.backgroundColor = background;
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
		this.titleNode.content = `${this.toolName} · ${phase}`;
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
			this.hiddenNode.visible = false;
			this.applyDetailAnimation();
			return;
		}
		const resultContent = textContent(this.result.content);
		const output = preview(resultContent, this.expanded);
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
			this.outputNode.content = output.content;
			this.outputNode.fg = this.result.isError
				? this.viewTheme.getFgColor("error")
				: this.viewTheme.getFgColor("toolOutput");
		}
		this.outputRoot.visible = Boolean(resultContent);
		if (
			this.renderedAttachments.length !== this.attachments.length ||
			this.renderedAttachments.some((attachment, index) => attachment !== this.attachments[index])
		) {
			clearChildren(this.attachmentsRoot);
			appendImageAttachments(this.renderer, this.attachmentsRoot, this.attachments, this.viewTheme);
			this.renderedAttachments = this.attachments;
		}
		this.attachmentsRoot.visible = this.attachments.length > 0;
		this.hiddenNode.content = output.hiddenLines > 0 && !diff ? `${output.hiddenLines} earlier lines hidden` : "";
		this.hiddenNode.fg = this.viewTheme.getFgColor("muted");
		this.hiddenNode.visible = Boolean(this.hiddenNode.content);
		this.applyDetailAnimation();
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

interface WorkingGroupEntry {
	id: string;
	view: OpenTUIWorkingGroupTool;
	complete: boolean;
	failed: boolean;
}

function workingGroupActivity(entries: readonly WorkingGroupEntry[], complete: boolean, failed: boolean): string {
	const kinds = new Set(entries.map((entry) => entry.view.getActivityKind?.() ?? "other"));
	if (kinds.size === 1) {
		const kind = kinds.values().next().value as OpenTUIToolActivityKind;
		if (failed) {
			if (kind === "inspect") return "Inspection failed";
			if (kind === "update") return "Update failed";
			if (kind === "command") return "Command failed";
			return "Work failed";
		}
		if (kind === "inspect") return complete ? "Inspected the workspace" : "Inspecting the workspace";
		if (kind === "update") {
			return complete
				? entries.length === 1
					? "Updated a file"
					: "Updated files"
				: entries.length === 1
					? "Updating a file"
					: "Updating files";
		}
		if (kind === "command") {
			return complete
				? entries.length === 1
					? "Ran a command"
					: "Ran commands"
				: entries.length === 1
					? "Running a command"
					: "Running commands";
		}
	}
	if (failed) return "Work failed";
	if (kinds.size === 2 && kinds.has("inspect") && kinds.has("update")) {
		return complete ? "Inspected and updated files" : "Inspecting and updating files";
	}
	return complete ? "Completed work" : "Working";
}

export class OpenTUIWorkingGroup extends RebuildableView {
	private readonly startedAt: number;
	private readonly now: () => number;
	private readonly entries: WorkingGroupEntry[] = [];
	private readonly viewTheme: Theme;
	private readonly header: BoxRenderable;
	private readonly summaryNode: TextRenderable;
	private readonly detailsRoot: BoxRenderable;
	private expanded = false;
	private completedAt: number | undefined;
	private activityMessage: string | undefined;
	private frame = 0;
	private readonly clicks = new OpenTUIClickCoordinator();
	private detailProgress = 0;
	private detailTarget = 0;
	private animationTimer: ReturnType<typeof setInterval> | undefined;
	private completeWithTools = true;
	private failed = false;

	constructor(renderer: CliRenderer, startedAt = Date.now(), now: () => number = Date.now, viewTheme: Theme = theme) {
		super(renderer);
		this.startedAt = startedAt;
		this.now = now;
		this.viewTheme = viewTheme;
		this.root.onMouse = (event) => {
			if (this.clicks.handle(event) && event.type === "down") this.renderer.clearSelection();
		};
		this.root.add(new BoxRenderable(renderer, { width: "100%", height: 1 }));
		this.header = new BoxRenderable(renderer, {
			flexDirection: "column",
			paddingX: 1,
		});
		this.clicks.register(this.header, () => this.toggleExpanded());
		this.summaryNode = new TextRenderable(renderer, { content: "" });
		this.header.add(this.summaryNode);
		this.detailsRoot = new BoxRenderable(renderer, { flexDirection: "column", visible: false, opacity: 0 });
		this.root.add(this.header);
		this.root.add(this.detailsRoot);
		this.rebuild();
		this.startAnimation();
	}

	addTool(id: string, view: OpenTUIWorkingGroupTool): void {
		if (this.entries.some((entry) => entry.id === id)) return;
		if (this.completeWithTools && this.completedAt !== undefined) {
			this.completedAt = undefined;
			this.startAnimation();
		}
		this.entries.push({ id, view, complete: false, failed: false });
		view.root.visible = false;
		this.detailsRoot.add(view.root);
		this.rebuild();
	}

	markToolComplete(id: string, failed: boolean): void {
		const entry = this.entries.find((candidate) => candidate.id === id);
		if (!entry) return;
		entry.complete = true;
		entry.failed = failed;
		if (failed) {
			entry.view.setExpanded(true);
			this.setExpanded(true);
		}
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

	finish(failed = false): void {
		this.failed ||= failed;
		if (this.completedAt !== undefined) {
			this.rebuild();
			return;
		}
		this.completedAt = this.now();
		if (this.failed || this.entries.some((entry) => entry.failed)) this.setExpanded(true);
		else this.setExpanded(false);
		this.rebuild();
	}

	setExpanded(expanded: boolean): void {
		if (this.expanded === expanded && this.detailTarget === (expanded ? 1 : 0)) return;
		this.expanded = expanded;
		this.detailTarget = expanded ? 1 : 0;
		if (expanded) this.detailsRoot.visible = true;
		this.rebuild();
		this.advanceAnimation();
		this.startAnimation();
	}

	setToolDetailsExpanded(expanded: boolean): void {
		this.setExpanded(expanded);
		for (const entry of this.entries) entry.view.setExpanded(expanded);
	}

	toggleExpanded(): void {
		this.setExpanded(!this.expanded);
	}

	isComplete(): boolean {
		return this.completedAt !== undefined;
	}

	protected rebuild(): void {
		if (this.root.isDestroyed) return;
		const failed = this.failed || this.entries.some((entry) => entry.failed);
		const count = this.entries.length;
		const elapsedSeconds = Math.max(1, Math.round(((this.completedAt ?? this.now()) - this.startedAt) / 1000));
		const toolCount = `${count} tool ${count === 1 ? "call" : "calls"}`;
		const activity =
			this.completedAt !== undefined && failed
				? workingGroupActivity(this.entries, true, true)
				: (this.activityMessage ?? workingGroupActivity(this.entries, this.completedAt !== undefined, failed));
		const suffix = count > 0 ? ` · ${toolCount}` : "";
		if (this.completedAt !== undefined) {
			this.summaryNode.content = `${this.expanded ? "⌄" : "›"} ${failed ? "✗" : "✓"} ${activity} · ${elapsedSeconds}s${suffix}`;
			this.summaryNode.fg = this.viewTheme.getFgColor(failed ? "error" : "muted");
			this.summaryNode.attributes = TextAttributes.NONE;
		} else {
			const prefix = `${this.expanded ? "⌄" : "›"} ${["◐", "◓", "◑", "◒"][this.frame % 4]} `;
			const highlight = this.frame % Math.max(1, activity.length + 4);
			const chunks = [fg(this.viewTheme.getFgColor("muted"))(prefix)];
			for (let index = 0; index < activity.length; index++) {
				chunks.push(
					fg(this.viewTheme.getFgColor(Math.abs(index - highlight) <= 1 ? "accent" : "toolTitle"))(
						activity[index] ?? "",
					),
				);
			}
			if (suffix) chunks.push(fg(this.viewTheme.getFgColor("muted"))(suffix));
			this.summaryNode.content = new StyledText(chunks);
			this.summaryNode.attributes = TextAttributes.BOLD;
		}
		this.applyDetailAnimation();
	}

	private startAnimation(): void {
		if (this.animationTimer) return;
		if (this.completedAt !== undefined && this.detailProgress === this.detailTarget) return;
		this.animationTimer = setInterval(() => this.advanceAnimation(), 60);
		(this.animationTimer as { unref?: () => void }).unref?.();
	}

	private advanceAnimation(): void {
		if (this.root.isDestroyed) {
			if (this.animationTimer) clearInterval(this.animationTimer);
			this.animationTimer = undefined;
			return;
		}
		this.frame++;
		if (this.detailProgress !== this.detailTarget) {
			const direction = this.detailTarget > this.detailProgress ? 1 : -1;
			this.detailProgress = Math.max(0, Math.min(1, this.detailProgress + direction * 0.25));
		}
		this.rebuild();
		this.renderer.requestRender();
		if (this.completedAt !== undefined && this.detailProgress === this.detailTarget && this.animationTimer) {
			clearInterval(this.animationTimer);
			this.animationTimer = undefined;
		}
	}

	private applyDetailAnimation(): void {
		this.detailsRoot.opacity = this.detailProgress;
		this.detailsRoot.visible = this.detailProgress > 0;
		const visibleCount = Math.ceil(this.entries.length * this.detailProgress);
		for (const [index, entry] of this.entries.entries()) entry.view.root.visible = index < visibleCount;
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
		if (!this.root.visible) return;
		const spinner = this.active ? ["◐", "◓", "◑", "◒"][this.frame % 4] : "·";
		this.root.add(
			new TextRenderable(this.renderer, {
				content: `${spinner} ${this.message}`,
				paddingX: 1,
				fg: this.viewTheme.getFgColor(this.kind === "retry" ? "warning" : "accent"),
			}),
		);
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
