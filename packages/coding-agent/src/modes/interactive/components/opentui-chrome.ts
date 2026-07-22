import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import { type Theme, theme } from "../theme/theme.ts";

export interface OpenTUITopBarState {
	conversation: string;
	workspace: string;
	model: string;
	thinking: string;
}

export class OpenTUITopBar implements BoneView {
	// biome-ignore lint/complexity/noUselessConstructor: Preserve the mode call site while the top bar is removed.
	constructor(_state: OpenTUITopBarState, _barTheme: Theme = theme) {}

	mount(context: BoneRenderContext): BoneNode {
		return context.createBox({ width: "100%", height: 0 });
	}

	update(_state: OpenTUITopBarState): void {}

	updateTheme(_nextTheme: Theme): void {}

	dispose(): void {}
}

export interface OpenTUIWelcomeOptions {
	workspace?: string;
	theme?: Theme;
}

export class OpenTUIWelcome implements BoneView {
	private readonly options: OpenTUIWelcomeOptions;
	private root: BoneContainerNode | undefined;
	private dismissed = false;

	constructor(options: OpenTUIWelcomeOptions = {}) {
		this.options = options;
	}

	mount(context: BoneRenderContext): BoneNode {
		const welcomeTheme = this.options.theme ?? theme;
		const root = context.createBox({ width: "100%", flexDirection: "column", paddingX: 1, paddingTop: 1 });
		root.append(
			context.createText({
				content: "What would you like to work on?",
				fg: welcomeTheme.getFgColor("text"),
			}),
		);
		if (this.options.workspace) {
			root.append(
				context.createText({
					content: this.options.workspace,
					fg: welcomeTheme.getFgColor("muted"),
					truncate: true,
				}),
			);
		}
		root.visible = !this.dismissed;
		this.root = root;
		return root;
	}

	dismiss(): void {
		this.dismissed = true;
		if (this.root && !this.root.destroyed) this.root.visible = false;
	}
}
