import { BoxRenderable, type CliRenderer, TextRenderable } from "@opentui/core";
import { type Theme, theme } from "../theme/theme.ts";

export interface OpenTUITopBarState {
	conversation: string;
	workspace: string;
	model: string;
	thinking: string;
}

export class OpenTUITopBar {
	readonly root: BoxRenderable;

	constructor(renderer: CliRenderer, _state: OpenTUITopBarState, _barTheme: Theme = theme) {
		this.root = new BoxRenderable(renderer, { width: "100%", height: 0 });
	}

	update(_state: OpenTUITopBarState): void {}

	updateTheme(_nextTheme: Theme): void {}

	dispose(): void {}
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
