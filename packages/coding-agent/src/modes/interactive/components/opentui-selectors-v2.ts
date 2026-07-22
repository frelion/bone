import type { ThinkingLevel } from "@frelion/bone-agent-core";
import type { BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import {
	getProjectTrustOptions,
	type ProjectTrustStoreEntry,
	type ProjectTrustUpdate,
} from "../../../core/trust-manager.ts";
import { getAvailableThemes, type Theme, theme } from "../theme/theme.ts";
import { type OpenTUISelectorAction, type OpenTUISelectorItem, OpenTUISelectorViewV2 } from "./opentui-selector-v2.ts";

const THINKING_DESCRIPTIONS: Record<ThinkingLevel, string> = {
	off: "No reasoning",
	minimal: "Very brief reasoning (~1k tokens)",
	low: "Light reasoning (~2k tokens)",
	medium: "Moderate reasoning (~8k tokens)",
	high: "Deep reasoning (~16k tokens)",
	xhigh: "Extra-high reasoning (~32k tokens)",
	max: "Maximum reasoning",
};

class SelectorFlowV2<T> implements BoneView {
	protected readonly selector: OpenTUISelectorViewV2<T>;

	constructor(selector: OpenTUISelectorViewV2<T>) {
		this.selector = selector;
	}

	mount(context: BoneRenderContext): BoneNode {
		return this.selector.mount(context);
	}

	handleAction(action: OpenTUISelectorAction): boolean {
		return this.selector.handleAction(action);
	}
}

export interface OpenTUIModelOptionV2 {
	provider: string;
	id: string;
	name: string;
	current?: boolean;
}

export type OpenTUIModelSelectionV2 = { kind: "model"; provider: string; id: string } | { kind: "follow-conversation" };

export class OpenTUIModelSelectorV2 extends SelectorFlowV2<OpenTUIModelSelectionV2> {
	constructor(options: {
		models: readonly OpenTUIModelOptionV2[];
		allowFollowConversation?: boolean;
		onSelect: (selection: OpenTUIModelSelectionV2) => void;
		onCancel: () => void;
		theme?: Theme;
	}) {
		const items: OpenTUISelectorItem<OpenTUIModelSelectionV2>[] = options.models.map((model) => ({
			value: { kind: "model", provider: model.provider, id: model.id },
			label: model.id,
			description: `[${model.provider}]${model.current ? " ✓" : ""}`,
			keywords: `${model.provider} ${model.name}`,
			current: model.current,
		}));
		if (options.allowFollowConversation) {
			items.unshift({
				value: { kind: "follow-conversation" },
				label: "Follow Conversation",
				description: "Use the current conversation model",
			});
		}
		super(
			new OpenTUISelectorViewV2({
				title: "Select model",
				subtitle: "Models from configured providers",
				items,
				searchable: true,
				searchPlaceholder: "Search models",
				theme: options.theme,
				onSelect: options.onSelect,
				onCancel: options.onCancel,
			}),
		);
	}
}

export class OpenTUIThemeSelectorV2 extends SelectorFlowV2<string> {
	constructor(options: {
		currentTheme: string;
		themes?: readonly string[];
		onSelect: (name: string) => void;
		onCancel: () => void;
		onPreview: (name: string) => void;
		theme?: Theme;
	}) {
		const themes = options.themes ?? getAvailableThemes();
		super(
			new OpenTUISelectorViewV2({
				title: "Select theme",
				items: themes.map((name) => ({ value: name, label: name, current: name === options.currentTheme })),
				selectedIndex: Math.max(0, themes.indexOf(options.currentTheme)),
				theme: options.theme,
				onSelect: options.onSelect,
				onCancel: options.onCancel,
				onPreview: options.onPreview,
			}),
		);
	}
}

export class OpenTUIThinkingSelectorV2 extends SelectorFlowV2<ThinkingLevel> {
	constructor(options: {
		currentLevel: ThinkingLevel;
		availableLevels: readonly ThinkingLevel[];
		onSelect: (level: ThinkingLevel) => void;
		onCancel: () => void;
		theme?: Theme;
	}) {
		super(
			new OpenTUISelectorViewV2({
				title: "Thinking level",
				items: options.availableLevels.map((level) => ({
					value: level,
					label: level,
					description: THINKING_DESCRIPTIONS[level],
					current: level === options.currentLevel,
				})),
				selectedIndex: Math.max(0, options.availableLevels.indexOf(options.currentLevel)),
				theme: options.theme,
				onSelect: options.onSelect,
				onCancel: options.onCancel,
			}),
		);
	}
}

export class OpenTUIShowImagesSelectorV2 extends SelectorFlowV2<boolean> {
	constructor(options: {
		currentValue: boolean;
		onSelect: (show: boolean) => void;
		onCancel: () => void;
		theme?: Theme;
	}) {
		super(
			new OpenTUISelectorViewV2({
				title: "Show images",
				items: [
					{ value: true, label: "Yes", description: "Show images inline in terminal" },
					{ value: false, label: "No", description: "Show text placeholder instead" },
				],
				selectedIndex: options.currentValue ? 0 : 1,
				theme: options.theme,
				onSelect: options.onSelect,
				onCancel: options.onCancel,
			}),
		);
	}
}

export interface OpenTUITrustSelectionV2 {
	trusted: boolean;
	updates: ProjectTrustUpdate[];
}

export class OpenTUITrustSelectorV2 extends SelectorFlowV2<OpenTUITrustSelectionV2> {
	constructor(options: {
		cwd: string;
		savedDecision: ProjectTrustStoreEntry | null;
		projectTrusted: boolean;
		onSelect: (selection: OpenTUITrustSelectionV2) => void;
		onCancel: () => void;
		theme?: Theme;
	}) {
		const trustOptions = getProjectTrustOptions(options.cwd);
		const savedIndex = trustOptions.findIndex(
			(option) =>
				option.savedPath !== undefined &&
				options.savedDecision?.decision === option.trusted &&
				options.savedDecision.path === option.savedPath,
		);
		super(
			new OpenTUISelectorViewV2({
				title: "Project trust",
				subtitle: `${options.cwd} · Current workspace: ${options.projectTrusted ? "trusted" : "untrusted"}`,
				items: trustOptions.map((option) => ({
					value: { trusted: option.trusted, updates: option.updates },
					label: option.label,
					current:
						option.savedPath !== undefined &&
						options.savedDecision?.decision === option.trusted &&
						options.savedDecision.path === option.savedPath,
				})),
				selectedIndex: Math.max(0, savedIndex),
				theme: options.theme ?? theme,
				onSelect: options.onSelect,
				onCancel: options.onCancel,
			}),
		);
	}
}
