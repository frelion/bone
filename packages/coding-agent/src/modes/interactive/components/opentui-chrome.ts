import type { BoneNode, BoneRenderContext, BoneTextNode, BoneUnsubscribe, BoneView } from "@frelion/bone-tui";
import { type Theme, theme } from "../theme/theme.ts";

export interface OpenTUITopBarState {
	conversation: string;
	workspace: string;
	model: string;
	thinking: string;
}

export class OpenTUITopBar implements BoneView {
	private state: OpenTUITopBarState;
	private barTheme: Theme;
	private conversationNode: BoneTextNode | undefined;
	private contextNode: BoneTextNode | undefined;
	private viewportWidth = 120;
	private unsubscribeResize: BoneUnsubscribe | undefined;

	constructor(state: OpenTUITopBarState, barTheme: Theme = theme) {
		this.state = state;
		this.barTheme = barTheme;
	}

	mount(context: BoneRenderContext): BoneNode {
		this.viewportWidth = context.width;
		const root = context.createBox({
			width: "100%",
			height: 1,
			flexDirection: "row",
			alignItems: "center",
			gap: 1,
			paddingX: 1,
		});
		root.append(
			context.createText({
				content: "BONE",
				fg: this.barTheme.getFgColor("accent"),
				bold: true,
				flexShrink: 0,
			}),
		);
		this.conversationNode = context.createText({
			content: "",
			fg: this.barTheme.getFgColor("text"),
			truncate: true,
			flexGrow: 1,
			minWidth: 0,
		});
		this.contextNode = context.createText({
			content: "",
			fg: this.barTheme.getFgColor("muted"),
			truncate: true,
			flexShrink: 1,
		});
		root.append(this.conversationNode);
		root.append(this.contextNode);
		this.unsubscribeResize = context.onResize((width) => {
			this.viewportWidth = width;
			this.refresh();
		});
		this.refresh();
		return root;
	}

	update(state: OpenTUITopBarState): void {
		this.state = state;
		this.refresh();
	}

	updateTheme(nextTheme: Theme): void {
		this.barTheme = nextTheme;
		this.conversationNode?.updateStyle({ fg: nextTheme.getFgColor("text") });
		this.contextNode?.updateStyle({ fg: nextTheme.getFgColor("muted") });
	}

	dispose(): void {
		this.unsubscribeResize?.();
		this.unsubscribeResize = undefined;
	}

	private refresh(): void {
		if (this.conversationNode) this.conversationNode.content = `/ ${this.state.conversation}`;
		if (!this.contextNode) return;
		this.contextNode.visible = this.viewportWidth >= 132;
		this.contextNode.content = `${this.state.workspace} · ${this.state.model} · ${this.state.thinking}`;
	}
}
