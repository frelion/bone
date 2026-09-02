import { type AssistantMessage, getTextContentPhase } from "@frelion/bone-ai";
import {
	BoxRenderable,
	type CliRenderer,
	CodeRenderable,
	type MarkdownOptions,
	MarkdownRenderable,
	StyledText,
	SyntaxStyle,
	fg as styledForeground,
	TextAttributes,
	type TextChunk,
	TextRenderable,
} from "@opentui/core";
import type { PlanProposal } from "../../../core/plan-mode.ts";
import { PROPOSED_PLAN_CLOSE_TAG, PROPOSED_PLAN_OPEN_TAG } from "../../../core/plan-mode.ts";
import { type Theme, theme } from "../theme/theme.ts";

function appendSpacer(renderer: CliRenderer, parent: BoxRenderable, size = 1): void {
	parent.add(new BoxRenderable(renderer, { width: "100%", height: size }));
}

function clearChildren(root: BoxRenderable): void {
	for (const child of root.getChildren()) child.destroyRecursively();
}

function markdownStyle(messageTheme: Theme, fg: string): SyntaxStyle {
	return SyntaxStyle.fromStyles({
		default: { fg },
		"markup.heading": { fg, bold: true },
		"markup.strong": { fg, bold: true },
		"markup.italic": { fg, italic: true },
		"markup.strikethrough": { fg, dim: true },
		"markup.link": { fg: "#5fafff", underline: true },
		"markup.raw": { fg: messageTheme.getFgColor("mdCodeBlock") },
	});
}

function markdownCodeBlockRenderer(
	renderer: CliRenderer,
	messageTheme: Theme,
	textColor: string,
): NonNullable<MarkdownOptions["renderNode"]> {
	return (token, context) => {
		if (token.type === "code") {
			const code = new CodeRenderable(renderer, {
				content: token.text,
				filetype: token.lang?.trim().split(/\s+/, 1)[0] || undefined,
				syntaxStyle: context.syntaxStyle,
				fg: messageTheme.getFgColor("mdCodeBlock"),
				drawUnstyledText: true,
				width: "100%",
			});
			const box = new BoxRenderable(renderer, {
				width: "100%",
				flexDirection: "column",
				border: ["left"],
				borderColor: messageTheme.getFgColor("mdCodeBlockBorder"),
				paddingX: 1,
				paddingY: 1,
			});
			box.add(code);
			return box;
		}
		if (token.type !== "paragraph" && token.type !== "heading" && token.type !== "text") return undefined;
		const content = new StyledText(
			inlineMarkdownChunks(
				"tokens" in token && Array.isArray(token.tokens) ? token.tokens : [],
				messageTheme,
				textColor,
				token.type === "heading" ? TextAttributes.BOLD : TextAttributes.NONE,
			),
		);
		return new TextRenderable(renderer, { content, width: "100%", wrapMode: "word" });
	};
}

function inlineMarkdownChunks(
	tokens: readonly unknown[],
	messageTheme: Theme,
	textColor: string,
	attributes = TextAttributes.NONE,
	linkUrl?: string,
): TextChunk[] {
	const chunks: TextChunk[] = [];
	for (const value of tokens) {
		if (typeof value !== "object" || value === null) continue;
		const token = value as Record<string, unknown>;
		const type = typeof token.type === "string" ? token.type : "";
		const text = typeof token.text === "string" ? token.text : "";
		const children = Array.isArray(token.tokens) ? token.tokens : [];
		if (type === "strong" || type === "em" || type === "del" || type === "link") {
			const nestedAttributes =
				attributes |
				(type === "strong"
					? TextAttributes.BOLD
					: type === "em"
						? TextAttributes.ITALIC
						: type === "del"
							? TextAttributes.STRIKETHROUGH
							: TextAttributes.UNDERLINE);
			chunks.push(
				...inlineMarkdownChunks(
					children,
					messageTheme,
					textColor,
					nestedAttributes,
					type === "link" && typeof token.href === "string" ? token.href : linkUrl,
				),
			);
			continue;
		}
		if (type === "br") {
			chunks.push(styledChunk("\n", textColor, attributes, linkUrl));
			continue;
		}
		if (children.length > 0) {
			chunks.push(...inlineMarkdownChunks(children, messageTheme, textColor, attributes, linkUrl));
			continue;
		}
		if (!text) continue;
		chunks.push(
			styledChunk(
				text,
				type === "codespan" ? messageTheme.getFgColor("mdCodeBlock") : textColor,
				attributes,
				linkUrl,
			),
		);
	}
	return chunks;
}

function styledChunk(text: string, color: string, attributes: number, linkUrl?: string): TextChunk {
	const chunk = styledForeground(color)(text);
	return {
		...chunk,
		attributes: (chunk.attributes ?? TextAttributes.NONE) | attributes,
		...(linkUrl ? { link: { url: linkUrl } } : {}),
	};
}

export function isOpenTUICommentaryText(
	content: Extract<AssistantMessage["content"][number], { type: "text" }>,
): boolean {
	return getTextContentPhase(content) === "commentary";
}

function getVisibleTextParts(message: AssistantMessage): Map<number, string> {
	const visibleByIndex = new Map<number, string>();
	let insidePlanBlock = false;

	for (let index = 0; index < message.content.length; index++) {
		const content = message.content[index];
		if (content.type !== "text" || isOpenTUICommentaryText(content)) continue;

		let remaining = content.text;
		let visible = "";
		while (remaining.length > 0) {
			if (insidePlanBlock) {
				const closeIndex = remaining.indexOf(PROPOSED_PLAN_CLOSE_TAG);
				if (closeIndex === -1) break;
				remaining = remaining.slice(closeIndex + PROPOSED_PLAN_CLOSE_TAG.length);
				insidePlanBlock = false;
				continue;
			}

			const openIndex = remaining.indexOf(PROPOSED_PLAN_OPEN_TAG);
			if (openIndex === -1) {
				visible += remaining;
				break;
			}
			visible += remaining.slice(0, openIndex);
			remaining = remaining.slice(openIndex + PROPOSED_PLAN_OPEN_TAG.length);
			insidePlanBlock = true;
		}
		visibleByIndex.set(index, visible);
	}

	return visibleByIndex;
}

export class OpenTUIUserMessage {
	readonly root: BoxRenderable;
	private readonly text: string;
	private readonly outputPad: number;
	private readonly messageTheme: Theme;

	constructor(renderer: CliRenderer, text: string, outputPad = 1, messageTheme: Theme = theme) {
		this.text = text;
		this.outputPad = outputPad;
		this.messageTheme = messageTheme;
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		appendSpacer(renderer, this.root);
		const body = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "column",
			paddingX: this.outputPad,
			backgroundColor: this.messageTheme.getBgColor("userMessageBg"),
		});
		body.add(
			new TextRenderable(renderer, {
				content: this.text,
				fg: this.messageTheme.getFgColor("userMessageText"),
				wrapMode: "word",
			}),
		);
		this.root.add(body);
	}
}

export interface OpenTUIAssistantMessageOptions {
	hideThinkingBlock?: boolean;
	hiddenThinkingLabel?: string;
	outputPad?: number;
	hideProposedPlan?: boolean;
	streaming?: boolean;
	theme?: Theme;
}

type AssistantSegmentKind = "text" | "commentary" | "thinking" | "thinking-label" | "error";

interface AssistantSegment {
	kind: AssistantSegmentKind;
	content: string;
}

interface AssistantSegmentOptions {
	hideThinkingBlock: boolean;
	hiddenThinkingLabel: string;
	hideProposedPlan: boolean;
}

function createAssistantSegments(message: AssistantMessage, options: AssistantSegmentOptions): AssistantSegment[] {
	const segments: AssistantSegment[] = [];
	const visibleTextParts = options.hideProposedPlan ? getVisibleTextParts(message) : undefined;
	for (let index = 0; index < message.content.length; index++) {
		const content = message.content[index]!;
		if (content.type === "text") {
			if (isOpenTUICommentaryText(content)) {
				if (content.text.trim()) segments.push({ kind: "commentary", content: content.text.trim() });
				continue;
			}
			const visibleText = visibleTextParts?.get(index) ?? content.text;
			const previous = segments.at(-1);
			if (previous?.kind === "text") previous.content += visibleText;
			else segments.push({ kind: "text", content: visibleText });
			continue;
		}
		if (content.type !== "thinking") continue;

		const thinkingBlocks: string[] = [];
		for (; index < message.content.length; index++) {
			const thinking = message.content[index];
			if (thinking?.type !== "thinking") break;
			if (thinking.thinking.trim()) thinkingBlocks.push(thinking.thinking.trim());
		}
		index--;
		if (thinkingBlocks.length > 0 && message.stopReason === undefined) {
			segments.push({
				kind: options.hideThinkingBlock ? "thinking-label" : "thinking",
				content: options.hideThinkingBlock ? options.hiddenThinkingLabel : thinkingBlocks.at(-1)!,
			});
		}
	}
	for (let index = segments.length - 1; index >= 0; index--) {
		const segment = segments[index]!;
		if (segment.kind !== "text") continue;
		segment.content = segment.content.trim();
		if (!segment.content) segments.splice(index, 1);
	}

	const hasToolCalls = message.content.some((content) => content.type === "toolCall");
	let error: string | undefined;
	if (message.stopReason === "length") {
		error = "Error: Model stopped because it reached the maximum output token limit. The response may be incomplete.";
	} else if (!hasToolCalls && message.stopReason === "aborted") {
		error =
			message.errorMessage && message.errorMessage !== "Request was aborted"
				? message.errorMessage
				: "Operation aborted";
	} else if (!hasToolCalls && message.stopReason === "error") {
		error = `Error: ${message.errorMessage || "Unknown error"}`;
	}
	if (error) segments.push({ kind: "error", content: error });
	return segments;
}

export function hasVisibleOpenTUIAssistantContent(
	message: AssistantMessage,
	options: Pick<OpenTUIAssistantMessageOptions, "hideThinkingBlock" | "hiddenThinkingLabel" | "hideProposedPlan"> = {},
): boolean {
	return (
		createAssistantSegments(message, {
			hideThinkingBlock: options.hideThinkingBlock ?? false,
			hiddenThinkingLabel: options.hiddenThinkingLabel ?? "Thinking...",
			hideProposedPlan: options.hideProposedPlan ?? false,
		}).length > 0
	);
}

export class OpenTUIAssistantMessage {
	readonly root: BoxRenderable;
	private message: AssistantMessage;
	private streaming: boolean;
	private readonly options: Required<Omit<OpenTUIAssistantMessageOptions, "streaming" | "theme">>;
	private readonly messageTheme: Theme;
	private readonly renderer: CliRenderer;
	private readonly syntaxStyle: SyntaxStyle;
	private readonly renderTextMarkdownNode: NonNullable<MarkdownOptions["renderNode"]>;
	private readonly renderThinkingMarkdownNode: NonNullable<MarkdownOptions["renderNode"]>;
	private renderedNodes: Array<MarkdownRenderable | TextRenderable> = [];

	constructor(renderer: CliRenderer, message: AssistantMessage, options: OpenTUIAssistantMessageOptions = {}) {
		this.renderer = renderer;
		this.message = message;
		this.streaming = options.streaming ?? message.stopReason === undefined;
		this.options = {
			hideThinkingBlock: options.hideThinkingBlock ?? false,
			hiddenThinkingLabel: options.hiddenThinkingLabel ?? "Thinking...",
			outputPad: options.outputPad ?? 1,
			hideProposedPlan: options.hideProposedPlan ?? false,
		};
		this.messageTheme = options.theme ?? theme;
		this.syntaxStyle = markdownStyle(this.messageTheme, this.messageTheme.getFgColor("text"));
		this.renderTextMarkdownNode = markdownCodeBlockRenderer(
			renderer,
			this.messageTheme,
			this.messageTheme.getFgColor("text"),
		);
		this.renderThinkingMarkdownNode = markdownCodeBlockRenderer(
			renderer,
			this.messageTheme,
			this.messageTheme.getFgColor("thinkingText"),
		);
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		this.rebuild();
	}

	updateContent(message: AssistantMessage, streaming = message.stopReason === undefined): void {
		this.message = message;
		this.streaming = streaming;
		this.rebuild();
	}

	private rebuild(): void {
		const root = this.root;
		if (root.isDestroyed) return;
		const segments = createAssistantSegments(this.message, this.options);
		const streaming = this.streaming;

		if (
			segments.length === this.renderedNodes.length &&
			segments.every((segment, index) => {
				const node = this.renderedNodes[index];
				if (!node || node.isDestroyed) return false;
				const usesTextRenderable = segment.kind === "thinking-label" || segment.kind === "error";
				return usesTextRenderable
					? node instanceof TextRenderable
					: node instanceof MarkdownRenderable && node.streaming === streaming;
			})
		) {
			for (let index = 0; index < segments.length; index++) {
				const node = this.renderedNodes[index];
				if (!node) continue;
				node.content = segments[index]!.content;
				if (node instanceof TextRenderable) {
					const kind = segments[index]!.kind;
					node.fg = this.messageTheme.getFgColor(kind === "error" ? "error" : "thinkingText");
					node.attributes = kind === "thinking-label" ? TextAttributes.ITALIC : TextAttributes.NONE;
				} else {
					node.fg = this.messageTheme.getFgColor(segments[index]!.kind === "thinking" ? "thinkingText" : "text");
					node.renderNode =
						segments[index]!.kind === "thinking" ? this.renderThinkingMarkdownNode : this.renderTextMarkdownNode;
					node.streaming = streaming;
				}
			}
			this.renderer.requestRender();
			return;
		}

		clearChildren(root);
		this.renderedNodes = [];
		if (segments.length === 0) {
			this.renderer.requestRender();
			return;
		}
		appendSpacer(this.renderer, root);
		for (const segment of segments) {
			if (segment.kind === "error" && segments.length > 1) appendSpacer(this.renderer, root);
			const node =
				segment.kind === "thinking-label" || segment.kind === "error"
					? new TextRenderable(this.renderer, {
							content: segment.content,
							paddingX: this.options.outputPad,
							fg: this.messageTheme.getFgColor(segment.kind === "error" ? "error" : "thinkingText"),
							attributes: segment.kind === "thinking-label" ? TextAttributes.ITALIC : TextAttributes.NONE,
							wrapMode: "word",
						})
					: new MarkdownRenderable(this.renderer, {
							content: segment.content,
							paddingX: this.options.outputPad,
							fg: this.messageTheme.getFgColor(segment.kind === "thinking" ? "thinkingText" : "text"),
							streaming,
							internalBlockMode: "top-level",
							syntaxStyle: this.syntaxStyle,
							renderNode:
								segment.kind === "thinking" ? this.renderThinkingMarkdownNode : this.renderTextMarkdownNode,
						});
			this.renderedNodes.push(node);
			root.add(node);
		}
		this.renderer.requestRender();
	}
}

export class OpenTUIPlanProposal {
	readonly root: BoxRenderable;
	private readonly proposal: PlanProposal;
	private readonly proposalTheme: Theme;

	constructor(renderer: CliRenderer, proposal: PlanProposal, proposalTheme: Theme = theme) {
		this.proposal = proposal;
		this.proposalTheme = proposalTheme;
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		appendSpacer(renderer, this.root);
		const box = new BoxRenderable(renderer, {
			flexDirection: "column",
			padding: 1,
			backgroundColor: this.proposalTheme.getBgColor("customMessageBg"),
		});
		box.add(
			new TextRenderable(renderer, {
				content: `Plan v${this.proposal.version}`,
				fg: this.proposalTheme.getFgColor("accent"),
				attributes: TextAttributes.BOLD,
			}),
		);
		appendSpacer(renderer, box);
		box.add(
			new MarkdownRenderable(renderer, {
				content: this.proposal.content,
				fg: this.proposalTheme.getFgColor("customMessageText"),
				internalBlockMode: "top-level",
				syntaxStyle: markdownStyle(this.proposalTheme, this.proposalTheme.getFgColor("customMessageText")),
				renderNode: markdownCodeBlockRenderer(
					renderer,
					this.proposalTheme,
					this.proposalTheme.getFgColor("customMessageText"),
				),
			}),
		);
		this.root.add(box);
	}
}
