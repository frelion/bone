import type { AssistantMessage } from "@frelion/bone-ai";
import {
	BoxRenderable,
	type CliRenderer,
	MarkdownRenderable,
	SyntaxStyle,
	TextAttributes,
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

function markdownStyle(fg: string): SyntaxStyle {
	return SyntaxStyle.fromStyles({
		default: { fg },
		"markup.heading": { fg, bold: true },
		"markup.link": { fg: "#5fafff", underline: true },
		"markup.raw": { fg: "#87d787" },
	});
}

function getVisibleTextParts(message: AssistantMessage): Map<number, string> {
	const visibleByIndex = new Map<number, string>();
	let insidePlanBlock = false;

	for (let index = 0; index < message.content.length; index++) {
		const content = message.content[index];
		if (content.type !== "text") continue;

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
	theme?: Theme;
}

type AssistantSegmentKind = "text" | "thinking" | "thinking-label" | "error";

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
			const visibleText = visibleTextParts?.get(index) ?? content.text;
			if (visibleText.trim()) segments.push({ kind: "text", content: visibleText.trim() });
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
	private readonly options: Required<Omit<OpenTUIAssistantMessageOptions, "theme">>;
	private readonly messageTheme: Theme;
	private readonly renderer: CliRenderer;
	private renderedNodes: Array<MarkdownRenderable | TextRenderable> = [];

	constructor(renderer: CliRenderer, message: AssistantMessage, options: OpenTUIAssistantMessageOptions = {}) {
		this.renderer = renderer;
		this.message = message;
		this.options = {
			hideThinkingBlock: options.hideThinkingBlock ?? false,
			hiddenThinkingLabel: options.hiddenThinkingLabel ?? "Thinking...",
			outputPad: options.outputPad ?? 1,
			hideProposedPlan: options.hideProposedPlan ?? false,
		};
		this.messageTheme = options.theme ?? theme;
		this.root = new BoxRenderable(renderer, { flexDirection: "column" });
		this.rebuild();
	}

	updateContent(message: AssistantMessage): void {
		this.message = message;
		this.rebuild();
	}

	private rebuild(): void {
		const root = this.root;
		if (root.isDestroyed) return;
		const segments = createAssistantSegments(this.message, this.options);

		if (
			segments.length === this.renderedNodes.length &&
			segments.every((segment, index) => {
				const node = this.renderedNodes[index];
				if (!node || node.isDestroyed) return false;
				const usesTextRenderable = segment.kind === "thinking-label" || segment.kind === "error";
				return usesTextRenderable ? node instanceof TextRenderable : node instanceof MarkdownRenderable;
			})
		) {
			for (let index = 0; index < segments.length; index++) {
				const node = this.renderedNodes[index];
				if (!node) continue;
				node.content = segments[index]!.content;
				if (node instanceof TextRenderable) {
					node.fg = this.messageTheme.getFgColor(segments[index]!.kind === "error" ? "error" : "thinkingText");
					node.attributes =
						segments[index]!.kind === "thinking-label" ? TextAttributes.ITALIC : TextAttributes.NONE;
				} else {
					node.fg = this.messageTheme.getFgColor(segments[index]!.kind === "thinking" ? "thinkingText" : "text");
					node.streaming = this.message.stopReason === undefined;
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
							streaming: this.message.stopReason === undefined,
							syntaxStyle: markdownStyle(this.messageTheme.getFgColor("text")),
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
				syntaxStyle: markdownStyle(this.proposalTheme.getFgColor("customMessageText")),
			}),
		);
		this.root.add(box);
	}
}
