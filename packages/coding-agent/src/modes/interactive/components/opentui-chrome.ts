import { BoxRenderable, type CliRenderer, TextAttributes, TextRenderable } from "@opentui/core";
import { type Theme, theme } from "../theme/theme.ts";

export interface OpenTUITopBarState {
	conversation: string;
	workspace: string;
	model: string;
	thinking: string;
}

export class OpenTUITopBar {
	readonly root: BoxRenderable;
	private readonly conversationNode: TextRenderable;
	private readonly workspaceNode: TextRenderable;
	private barTheme: Theme;

	constructor(renderer: CliRenderer, state: OpenTUITopBarState, barTheme: Theme = theme) {
		this.barTheme = barTheme;
		this.root = new BoxRenderable(renderer, {
			width: "100%",
			height: 1,
			flexDirection: "row",
			alignItems: "center",
			paddingX: 1,
		});
		this.conversationNode = new TextRenderable(renderer, {
			content: "",
			attributes: TextAttributes.BOLD,
			truncate: true,
			flexGrow: 1,
			minWidth: 0,
			height: 1,
		});
		this.workspaceNode = new TextRenderable(renderer, {
			content: "",
			truncate: true,
			flexShrink: 1,
			minWidth: 0,
			height: 1,
		});
		this.root.add(this.conversationNode);
		this.root.add(this.workspaceNode);
		this.updateTheme(barTheme);
		this.update(state);
	}

	update(state: OpenTUITopBarState): void {
		if (this.root.isDestroyed) return;
		this.conversationNode.content = state.conversation.trim() || "New conversation";
		this.workspaceNode.content = state.workspace.trim();
	}

	updateTheme(nextTheme: Theme): void {
		if (this.root.isDestroyed) return;
		this.barTheme = nextTheme;
		this.conversationNode.fg = this.barTheme.getFgColor("text");
		this.workspaceNode.fg = this.barTheme.getFgColor("muted");
	}

	dispose(): void {
		if (!this.root.isDestroyed) this.root.destroyRecursively();
	}
}

export interface OpenTUIWelcomeOptions {
	workspace?: string;
	theme?: Theme;
}

export class OpenTUIWelcome {
	readonly root: BoxRenderable;
	private readonly options: OpenTUIWelcomeOptions;
	private dismissed = false;

	constructor(renderer: CliRenderer, options: OpenTUIWelcomeOptions = {}) {
		this.options = options;
		const welcomeTheme = this.options.theme ?? theme;
		this.root = new BoxRenderable(renderer, { width: "100%", flexDirection: "column", paddingX: 1, paddingTop: 1 });
		this.root.add(
			new TextRenderable(renderer, {
				content: "What would you like to work on?",
				fg: welcomeTheme.getFgColor("text"),
			}),
		);
		if (this.options.workspace) {
			this.root.add(
				new TextRenderable(renderer, {
					content: this.options.workspace,
					fg: welcomeTheme.getFgColor("muted"),
					truncate: true,
				}),
			);
		}
		this.root.visible = !this.dismissed;
	}

	dismiss(): void {
		this.dismissed = true;
		if (!this.root.isDestroyed) this.root.visible = false;
	}
}
