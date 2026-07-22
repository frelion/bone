import type { AssistantMessage } from "@frelion/bone-ai";
import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
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
		const box = context.createBox({
			flexDirection: "column",
			paddingX: this.outputPad,
			paddingY: 1,
			backgroundColor: this.messageTheme.getBgColor("userMessageBg"),
		});
		box.append(
			context.createMarkdown({
				content: this.text,
				fg: this.messageTheme.getFgColor("userMessageText"),
			}),
		);
		return box;
	}
}

export interface OpenTUIAssistantMessageOptions {
	hideThinkingBlock?: boolean;
	hiddenThinkingLabel?: string;
	outputPad?: number;
	hideProposedPlan?: boolean;
	theme?: Theme;
}

export class OpenTUIAssistantMessage implements BoneView {
	private message: AssistantMessage;
	private readonly options: Required<Omit<OpenTUIAssistantMessageOptions, "theme">>;
	private readonly messageTheme: Theme;
	private context: BoneRenderContext | undefined;
	private root: BoneContainerNode | undefined;

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
		root.clear();

		const visibleTextParts = this.options.hideProposedPlan ? getVisibleTextParts(this.message) : undefined;
		const hasVisibleContent = this.message.content.some((content, index) =>
			content.type === "text"
				? (visibleTextParts?.get(index) ?? content.text).trim()
				: content.type === "thinking" && content.thinking.trim(),
		);
		if (hasVisibleContent) appendSpacer(context, root);

		for (let index = 0; index < this.message.content.length; index++) {
			const content = this.message.content[index]!;
			if (content.type === "text") {
				const visibleText = visibleTextParts?.get(index) ?? content.text;
				if (!visibleText.trim()) continue;
				root.append(
					context.createMarkdown({
						content: visibleText.trim(),
						paddingX: this.options.outputPad,
						fg: this.messageTheme.getFgColor("text"),
						streaming: this.message.stopReason === undefined,
					}),
				);
				continue;
			}
			if (content.type !== "thinking") continue;

			const thinkingBlocks: string[] = [];
			for (; index < this.message.content.length; index++) {
				const thinking = this.message.content[index];
				if (thinking?.type !== "thinking") break;
				if (thinking.thinking.trim()) thinkingBlocks.push(thinking.thinking.trim());
			}
			index--;
			if (thinkingBlocks.length === 0) continue;

			if (this.options.hideThinkingBlock) {
				root.append(
					context.createText({
						content: this.options.hiddenThinkingLabel,
						paddingX: this.options.outputPad,
						fg: this.messageTheme.getFgColor("thinkingText"),
						italic: true,
					}),
				);
			} else {
				root.append(
					context.createMarkdown({
						content: thinkingBlocks.join("\n\n"),
						paddingX: this.options.outputPad,
						fg: this.messageTheme.getFgColor("thinkingText"),
						streaming: this.message.stopReason === undefined,
					}),
				);
			}
		}

		const hasToolCalls = this.message.content.some((content) => content.type === "toolCall");
		let error: string | undefined;
		if (this.message.stopReason === "length") {
			error =
				"Error: Model stopped because it reached the maximum output token limit. The response may be incomplete.";
		} else if (!hasToolCalls && this.message.stopReason === "aborted") {
			error =
				this.message.errorMessage && this.message.errorMessage !== "Request was aborted"
					? this.message.errorMessage
					: "Operation aborted";
		} else if (!hasToolCalls && this.message.stopReason === "error") {
			error = `Error: ${this.message.errorMessage || "Unknown error"}`;
		}
		if (error) {
			appendSpacer(context, root);
			root.append(
				context.createText({
					content: error,
					paddingX: this.options.outputPad,
					fg: this.messageTheme.getFgColor("error"),
					wrapMode: "word",
				}),
			);
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
