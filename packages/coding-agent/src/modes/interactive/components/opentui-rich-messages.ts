import type { TextContent, ToolResultMessage } from "@frelion/bone-ai";
import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import type { ParsedSkillBlock } from "../../../core/agent-session.ts";
import type {
	BashExecutionMessage,
	BranchSummaryMessage,
	CompactionSummaryMessage,
	CustomMessage,
} from "../../../core/messages.ts";
import { stripAnsi } from "../../../utils/ansi.ts";
import { type Theme, theme } from "../theme/theme.ts";

const PREVIEW_LINES = 20;

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
	context: BoneRenderContext,
	body: BoneContainerNode,
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
			body.append(
				context.createImage({
					pixels: attachment.pixels,
					pixelWidth: attachment.pixelWidth,
					pixelHeight: attachment.pixelHeight,
					terminalWidth: attachment.terminalWidth,
					terminalHeight: attachment.terminalHeight,
				}),
			);
			continue;
		}
		body.append(
			context.createText({
				content: `[image: ${attachment.mimeType}; ${attachment.error ?? "unable to decode"}]`,
				fg: viewTheme.getFgColor("warning"),
				wrapMode: "word",
			}),
		);
	}
}

abstract class RebuildableView implements BoneView {
	protected context: BoneRenderContext | undefined;
	protected root: BoneContainerNode | undefined;

	mount(context: BoneRenderContext): BoneNode {
		this.context = context;
		this.root = context.createBox({ flexDirection: "column" });
		this.rebuild();
		return this.root;
	}

	protected abstract rebuild(): void;

	protected begin(backgroundColor?: string): { context: BoneRenderContext; body: BoneContainerNode } | undefined {
		if (!this.context || !this.root) return undefined;
		this.root.clear();
		this.root.append(this.context.createSpacer({ size: 1, direction: "vertical" }));
		const body = this.context.createBox({
			flexDirection: "column",
			paddingX: 1,
			paddingY: 1,
			backgroundColor,
		});
		this.root.append(body);
		return { context: this.context, body };
	}
}

export interface OpenTUIToolExecutionOptions {
	theme?: Theme;
	expanded?: boolean;
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

	constructor(toolName: string, toolCallId: string, args: unknown, options: OpenTUIToolExecutionOptions = {}) {
		super();
		this.toolName = toolName;
		this.toolCallId = toolCallId;
		this.args = args;
		this.expanded = options.expanded ?? false;
		this.viewTheme = options.theme ?? theme;
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
		this.expanded = expanded;
		this.rebuild();
	}

	protected rebuild(): void {
		const background = this.partial
			? this.viewTheme.getBgColor("toolPendingBg")
			: this.result?.isError
				? this.viewTheme.getBgColor("toolErrorBg")
				: this.viewTheme.getBgColor("toolSuccessBg");
		const mounted = this.begin(background);
		if (!mounted) return;
		const { context, body } = mounted;
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
		body.append(
			context.createText({
				content: `${this.toolName} · ${phase}`,
				fg: this.result?.isError ? this.viewTheme.getFgColor("error") : this.viewTheme.getFgColor("toolTitle"),
				bold: true,
			}),
		);
		const serializedArgs = JSON.stringify(this.args, null, 2);
		if (serializedArgs && serializedArgs !== "{}") {
			body.append(
				context.createText({
					content: serializedArgs,
					fg: this.viewTheme.getFgColor("toolOutput"),
					wrapMode: "word",
				}),
			);
		}
		if (!this.result) return;
		const resultContent = textContent(this.result.content);
		const output = preview(resultContent, this.expanded);
		if (resultContent && isUnifiedDiff(resultContent)) {
			body.append(
				context.createDiff({
					diff: resultContent,
					view: "unified",
					wrapMode: "word",
					showLineNumbers: true,
					fg: this.viewTheme.getFgColor("toolOutput"),
					addedSignColor: this.viewTheme.getFgColor("toolDiffAdded"),
					removedSignColor: this.viewTheme.getFgColor("toolDiffRemoved"),
				}),
			);
		} else if (output.content) {
			body.append(
				context.createText({
					content: output.content,
					fg: this.result.isError ? this.viewTheme.getFgColor("error") : this.viewTheme.getFgColor("toolOutput"),
					wrapMode: "word",
				}),
			);
		}
		appendImageAttachments(context, body, this.attachments, this.viewTheme);
		if (output.hiddenLines > 0 && !isUnifiedDiff(resultContent)) {
			body.append(
				context.createText({
					content: `${output.hiddenLines} earlier lines hidden`,
					fg: this.viewTheme.getFgColor("muted"),
					dim: true,
				}),
			);
		}
	}
}

export class OpenTUIBashExecution extends RebuildableView {
	private readonly command: string;
	private output = "";
	private status: "running" | "complete" | "cancelled" | "error" = "running";
	private exitCode: number | undefined;
	private expanded = false;
	private truncated = false;
	private fullOutputPath: string | undefined;
	private readonly excluded: boolean;
	private readonly viewTheme: Theme;

	constructor(command: string, excludeFromContext = false, viewTheme: Theme = theme) {
		super();
		this.command = command;
		this.excluded = excludeFromContext;
		this.viewTheme = viewTheme;
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
		const failed = this.status === "error";
		const mounted = this.begin(
			failed ? this.viewTheme.getBgColor("toolErrorBg") : this.viewTheme.getBgColor("customMessageBg"),
		);
		if (!mounted) return;
		const { context, body } = mounted;
		body.append(
			context.createText({
				content: `$ ${this.command}`,
				fg: this.viewTheme.getFgColor(this.excluded ? "dim" : "bashMode"),
				bold: true,
			}),
		);
		const output = preview(this.output, this.expanded);
		if (output.content)
			body.append(
				context.createText({ content: output.content, fg: this.viewTheme.getFgColor("muted"), wrapMode: "word" }),
			);
		const details: string[] = [];
		if (output.hiddenLines > 0) details.push(`${output.hiddenLines} earlier lines hidden`);
		if (this.status === "running") details.push("Running...");
		if (this.status === "cancelled") details.push("Cancelled");
		if (failed) details.push(`Exited with code ${this.exitCode}`);
		if (this.truncated && this.fullOutputPath) details.push(`Output truncated: ${this.fullOutputPath}`);
		if (details.length > 0) {
			body.append(
				context.createText({
					content: details.join("\n"),
					fg: failed ? this.viewTheme.getFgColor("error") : this.viewTheme.getFgColor("muted"),
					dim: !failed,
				}),
			);
		}
	}
}

export type OpenTUIStatusKind = "working" | "retry" | "compaction" | "branchSummary";

export class OpenTUIStatusView extends RebuildableView {
	private message: string;
	private frame = 0;
	private active = true;
	private readonly kind: OpenTUIStatusKind;
	private viewTheme: Theme;

	constructor(kind: OpenTUIStatusKind, message: string, viewTheme: Theme = theme) {
		super();
		this.kind = kind;
		this.message = message;
		this.viewTheme = viewTheme;
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
		if (!this.context || !this.root) return;
		this.root.clear();
		this.root.visible = this.message !== "Ready";
		if (!this.root.visible) return;
		const spinner = this.active ? ["◐", "◓", "◑", "◒"][this.frame % 4] : "·";
		this.root.append(
			this.context.createText({
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

	constructor(viewTheme: Theme) {
		super();
		this.viewTheme = viewTheme;
	}

	setExpanded(expanded: boolean): void {
		this.expanded = expanded;
		this.rebuild();
	}

	protected renderSummary(label: string, collapsed: string, markdown: string): void {
		const mounted = this.begin(this.viewTheme.getBgColor("customMessageBg"));
		if (!mounted) return;
		mounted.body.append(
			mounted.context.createText({
				content: `[${label}]`,
				fg: this.viewTheme.getFgColor("customMessageLabel"),
				bold: true,
			}),
		);
		if (this.expanded) {
			mounted.body.append(
				mounted.context.createMarkdown({ content: markdown, fg: this.viewTheme.getFgColor("customMessageText") }),
			);
		} else {
			mounted.body.append(
				mounted.context.createText({ content: collapsed, fg: this.viewTheme.getFgColor("customMessageText") }),
			);
		}
	}
}

export class OpenTUICompactionSummary extends ExpandableSummaryView {
	private readonly message: CompactionSummaryMessage;

	constructor(message: CompactionSummaryMessage, viewTheme: Theme = theme) {
		super(viewTheme);
		this.message = message;
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

	constructor(message: BranchSummaryMessage, viewTheme: Theme = theme) {
		super(viewTheme);
		this.message = message;
	}

	protected rebuild(): void {
		this.renderSummary("branch", "Branch summary", `**Branch Summary**\n\n${this.message.summary}`);
	}
}

export class OpenTUISkillInvocation extends ExpandableSummaryView {
	private readonly skill: ParsedSkillBlock;

	constructor(skill: ParsedSkillBlock, viewTheme: Theme = theme) {
		super(viewTheme);
		this.skill = skill;
	}

	protected rebuild(): void {
		this.renderSummary("skill", this.skill.name, `**${this.skill.name}**\n\n${this.skill.content}`);
	}
}

export class OpenTUICustomMessage extends ExpandableSummaryView {
	private message: CustomMessage<unknown>;

	constructor(message: CustomMessage<unknown>, viewTheme: Theme = theme) {
		super(viewTheme);
		this.message = message;
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
