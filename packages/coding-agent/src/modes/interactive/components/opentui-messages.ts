import type { AssistantMessage } from "@frelion/bone-ai";
import type {
	BoneContainerNode,
	BoneMarkdownNode,
	BoneNode,
	BoneRenderContext,
	BoneTextNode,
	BoneView,
} from "@frelion/bone-tui";
import type { PlanProposal } from "../../../core/plan-mode.ts";
import { PROPOSED_PLAN_CLOSE_TAG, PROPOSED_PLAN_OPEN_TAG } from "../../../core/plan-mode.ts";
import { type Theme, theme } from "../theme/theme.ts";

function appendSpacer(context: BoneRenderContext, parent: BoneContainerNode, size = 1): void {
	parent.append(context.createSpacer({ size, direction: "vertical" }));
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

export class OpenTUIUserMessage implements BoneView {
	private readonly text: string;
	private readonly outputPad: number;
	private readonly messageTheme: Theme;

	constructor(text: string, outputPad = 1, messageTheme: Theme = theme) {
		this.text = text;
		this.outputPad = outputPad;
		this.messageTheme = messageTheme;
	}

	mount(context: BoneRenderContext): BoneNode {
		const root = context.createBox({ flexDirection: "column" });
		appendSpacer(context, root);
		const body = context.createBox({ flexDirection: "column", paddingX: this.outputPad });
		body.append(
			context.createText({
				content: `› ${this.text}`,
				fg: this.messageTheme.getFgColor("userMessageText"),
				wrapMode: "word",
			}),
		);
		root.append(body);
		return root;
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

export class OpenTUIAssistantMessage implements BoneView {
	private message: AssistantMessage;
	private readonly options: Required<Omit<OpenTUIAssistantMessageOptions, "theme">>;
	private readonly messageTheme: Theme;
	private context: BoneRenderContext | undefined;
	private root: BoneContainerNode | undefined;
	private renderedKinds: AssistantSegmentKind[] = [];
	private renderedNodes: Array<BoneMarkdownNode | BoneTextNode> = [];

	constructor(message: AssistantMessage, options: OpenTUIAssistantMessageOptions = {}) {
		this.message = message;
		this.options = {
			hideThinkingBlock: options.hideThinkingBlock ?? false,
			hiddenThinkingLabel: options.hiddenThinkingLabel ?? "Thinking...",
			outputPad: options.outputPad ?? 1,
			hideProposedPlan: options.hideProposedPlan ?? false,
		};
		this.messageTheme = options.theme ?? theme;
	}

	mount(context: BoneRenderContext): BoneNode {
		this.context = context;
		this.root = context.createBox({ flexDirection: "column" });
		this.rebuild();
		return this.root;
	}

	updateContent(message: AssistantMessage): void {
		this.message = message;
		this.rebuild();
	}

	private rebuild(): void {
		const context = this.context;
		const root = this.root;
		if (!context || !root) return;
		const segments = createAssistantSegments(this.message, this.options);

		const kinds = segments.map((segment) => segment.kind);
		if (
			kinds.length === this.renderedKinds.length &&
			kinds.every((kind, index) => kind === this.renderedKinds[index]) &&
			this.renderedNodes.every((node) => !node.destroyed)
		) {
			for (let index = 0; index < segments.length; index++) {
				const node = this.renderedNodes[index];
				if (!node) continue;
				node.content = segments[index]!.content;
				if ("streaming" in node) node.streaming = this.message.stopReason === undefined;
			}
			return;
		}

		root.clear();
		this.renderedKinds = kinds;
		this.renderedNodes = [];
		if (segments.length === 0) return;
		appendSpacer(context, root);
		for (const segment of segments) {
			if (segment.kind === "error" && segments.length > 1) appendSpacer(context, root);
			const node =
				segment.kind === "thinking-label" || segment.kind === "error"
					? context.createText({
							content: segment.content,
							paddingX: this.options.outputPad,
							fg: this.messageTheme.getFgColor(segment.kind === "error" ? "error" : "thinkingText"),
							italic: segment.kind === "thinking-label",
							wrapMode: "word",
						})
					: context.createMarkdown({
							content: segment.content,
							paddingX: this.options.outputPad,
							fg: this.messageTheme.getFgColor(segment.kind === "thinking" ? "thinkingText" : "text"),
							streaming: this.message.stopReason === undefined,
						});
			this.renderedNodes.push(node);
			root.append(node);
		}
	}
}

export class OpenTUIPlanProposal implements BoneView {
	private readonly proposal: PlanProposal;
	private readonly proposalTheme: Theme;

	constructor(proposal: PlanProposal, proposalTheme: Theme = theme) {
		this.proposal = proposal;
		this.proposalTheme = proposalTheme;
	}

	mount(context: BoneRenderContext): BoneNode {
		const root = context.createBox({ flexDirection: "column" });
		appendSpacer(context, root);
		const box = context.createBox({
			flexDirection: "column",
			padding: 1,
			backgroundColor: this.proposalTheme.getBgColor("customMessageBg"),
		});
		box.append(
			context.createText({
				content: `Plan v${this.proposal.version}`,
				fg: this.proposalTheme.getFgColor("accent"),
				bold: true,
			}),
		);
		appendSpacer(context, box);
		box.append(
			context.createMarkdown({
				content: this.proposal.content,
				fg: this.proposalTheme.getFgColor("customMessageText"),
			}),
		);
		root.append(box);
		return root;
	}
}
