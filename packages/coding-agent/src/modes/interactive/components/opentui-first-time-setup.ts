import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import { APP_NAME } from "../../../config.ts";
import { setTheme, type TerminalTheme } from "../theme/theme.ts";
import { OpenTUISelectorViewV2 } from "./opentui-selector-v2.ts";

export interface FirstTimeSetupResult {
	theme: TerminalTheme;
	shareAnalytics: boolean;
}

export interface OpenTUIFirstTimeSetupOptions {
	detectedTheme: TerminalTheme;
	onSubmit: (result: FirstTimeSetupResult) => void;
	onCancel: () => void;
}

/** Two-step first-run setup implemented with structured selectors. */
export class OpenTUIFirstTimeSetupV2 implements BoneView {
	private readonly options: OpenTUIFirstTimeSetupOptions;
	private root: BoneContainerNode | undefined;
	private context: BoneRenderContext | undefined;
	private selector: OpenTUISelectorViewV2<TerminalTheme | boolean> | undefined;
	private selectedTheme: TerminalTheme;

	constructor(options: OpenTUIFirstTimeSetupOptions) {
		this.options = options;
		this.selectedTheme = options.detectedTheme;
	}

	mount(context: BoneRenderContext): BoneNode {
		this.context = context;
		this.root = context.createBox({ width: "100%", height: "100%", alignItems: "center", justifyContent: "center" });
		this.showThemeStep();
		return this.root;
	}

	handleAction(action: "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown"): boolean {
		return this.selector?.handleAction(action) ?? false;
	}

	private showThemeStep(): void {
		this.selector = new OpenTUISelectorViewV2<TerminalTheme | boolean>({
			title: `Welcome to ${APP_NAME}`,
			subtitle: `Pick a theme · detected ${this.options.detectedTheme}`,
			items: [
				{ value: "dark", label: "Dark" },
				{ value: "light", label: "Light" },
			],
			selectedIndex: this.options.detectedTheme === "light" ? 1 : 0,
			onPreview: (value) => {
				if (typeof value === "string") setTheme(value);
			},
			onSelect: (value) => {
				if (typeof value !== "string") return;
				this.selectedTheme = value;
				setTheme(value);
				this.showAnalyticsStep();
			},
			onCancel: this.options.onCancel,
		});
		this.replaceSelector();
	}

	private showAnalyticsStep(): void {
		this.selector = new OpenTUISelectorViewV2<TerminalTheme | boolean>({
			title: "Anonymous usage data",
			subtitle: "Share anonymous diagnostics when Bone telemetry is configured?",
			items: [
				{ value: true, label: "Share anonymous usage data" },
				{ value: false, label: "Don't share" },
			],
			onSelect: (value) => {
				if (typeof value === "boolean") this.options.onSubmit({ theme: this.selectedTheme, shareAnalytics: value });
			},
			onCancel: this.options.onCancel,
		});
		this.replaceSelector();
	}

	private replaceSelector(): void {
		if (!this.root || !this.selector) return;
		this.root.clear();
		this.root.append(this.selector.mount(this.requireContext()));
	}

	private requireContext(): BoneRenderContext {
		if (!this.context) throw new Error("OpenTUIFirstTimeSetupV2 must be mounted first");
		return this.context;
	}
}
