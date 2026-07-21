import type { Credential } from "@frelion/bone-ai";
import { type Component, type Focusable, getKeybindings, Input, truncateToWidth } from "@frelion/bone-tui";
import type { ModelsJson, ModelsJsonModel } from "../../../core/model-config.ts";
import type { ExtensionProviderRuntimeStatus } from "../../../core/model-runtime.ts";
import type { ProviderPreset } from "../../../core/provider-presets.ts";
import type { Settings, SettingsManager, SettingsScope } from "../../../core/settings-manager.ts";
import { theme } from "../theme/theme.ts";
import { createResourceSettingsList, type ResourceSettingsList, type ScopedResolvedPaths } from "./config-selector.ts";
import { ModalShell } from "./modal-shell.ts";
import {
	type ModelDetailItem,
	ModelSettingsDetail,
	ModelsProvidersBrowser,
	type ModelsProvidersEntry,
	type ProviderAuthenticationStatus,
	type ProviderDetailItem,
	ProviderSettingsDetail,
} from "./model-settings-navigator.ts";
import { ProviderFormComponent, type ProviderFormDraft } from "./provider-form.ts";
import { alignEnd, joinColumns } from "./terminal-layout.ts";

type PageId = "providers" | "defaults" | "delivery" | "appearance" | "tools" | "resources" | "security";
type SettingsFocus = "scope" | "navigation" | "content" | "actions";
export type SettingsProviderErrorTarget = {
	providerId?: string;
	modelId?: string;
	overrideId?: string;
	field?: string;
};

/** A save failure that can return the modal directly to the affected page. */
export class SettingsCenterSaveError extends Error {
	readonly page: PageId;
	readonly rowIndex: number | undefined;
	readonly providerTarget: SettingsProviderErrorTarget | undefined;

	constructor(page: PageId, message: string, rowIndex?: number, providerTarget?: SettingsProviderErrorTarget) {
		super(message);
		this.name = "SettingsCenterSaveError";
		this.page = page;
		this.rowIndex = rowIndex;
		this.providerTarget = providerTarget;
	}
}
type EditorStep =
	| "providerId"
	| "providerName"
	| "providerBaseUrl"
	| "providerApi"
	| "modelProvider"
	| "modelId"
	| "modelName"
	| "modelApi"
	| "modelContext"
	| "modelMaxTokens"
	| "editProvider"
	| "editModelId"
	| "editProviderName"
	| "editProviderBaseUrl"
	| "editProviderApi"
	| "editProviderAuthHeader"
	| "editProviderOauth"
	| "providerCredentialApiKey"
	| "editModelName"
	| "editModelApi"
	| "editModelBaseUrl"
	| "editModelReasoning"
	| "editModelInput"
	| "editModelContext"
	| "editModelMaxTokens"
	| "headerProvider"
	| "headerModelId"
	| "headerName"
	| "headerValue"
	| "advancedProvider"
	| "advancedModelId"
	| "advancedThinkingOff"
	| "advancedThinkingMinimal"
	| "advancedThinkingLow"
	| "advancedThinkingMedium"
	| "advancedThinkingHigh"
	| "advancedThinkingXhigh"
	| "advancedThinkingMax"
	| "advancedCostInput"
	| "advancedCostOutput"
	| "advancedCostCacheRead"
	| "advancedCostCacheWrite"
	| "advancedTierAction"
	| "advancedTierThreshold"
	| "advancedTierInput"
	| "advancedTierOutput"
	| "advancedTierCacheRead"
	| "advancedTierCacheWrite"
	| "compatProvider"
	| "compatModelId"
	| "compatField"
	| "compatTemplateName"
	| "compatValue"
	| "overrideProvider"
	| "overrideId"
	| "overrideName"
	| "overrideReasoning"
	| "overrideInput"
	| "overrideContext"
	| "overrideMaxTokens"
	| "settingValue";

interface Page {
	id: PageId;
	label: string;
}

/** A staged, secret-bearing mutation for one Provider's auth.json credential. */
export interface ProviderCredentialMutation {
	providerId: string;
	credential: Credential | undefined;
}

type CostEditorDraft = {
	input?: number;
	output?: number;
	cacheRead?: number;
	cacheWrite?: number;
};

type CostTierEditorDraft = {
	inputTokensAbove?: number;
	input?: number;
	output?: number;
	cacheRead?: number;
	cacheWrite?: number;
};

type ModelDeletionTarget =
	| { kind: "provider"; providerId: string }
	| { kind: "model"; providerId: string; modelId: string }
	| { kind: "override"; providerId: string; overrideId: string };

type SettingsActionMenu = {
	title: string;
	subtitle?: string;
	items: Array<{ label: string; action: () => void }>;
	index: number;
};

interface SettingsRow {
	label: string;
	value: string;
	action: () => void;
}

const THINKING_EDITOR_STEPS = [
	"advancedThinkingOff",
	"advancedThinkingMinimal",
	"advancedThinkingLow",
	"advancedThinkingMedium",
	"advancedThinkingHigh",
	"advancedThinkingXhigh",
	"advancedThinkingMax",
] as const;

const THINKING_LEVEL_BY_EDITOR_STEP: Record<(typeof THINKING_EDITOR_STEPS)[number], string> = {
	advancedThinkingOff: "off",
	advancedThinkingMinimal: "minimal",
	advancedThinkingLow: "low",
	advancedThinkingMedium: "medium",
	advancedThinkingHigh: "high",
	advancedThinkingXhigh: "xhigh",
	advancedThinkingMax: "max",
};

type CompatValueKind = "boolean" | "enum" | "list" | "number" | "numberOrString" | "stringOrNull";

interface CompatField {
	key: string;
	kind: CompatValueKind;
	values?: readonly string[];
}

const COMPAT_FIELDS: readonly CompatField[] = [
	{ key: "supportsStore", kind: "boolean" },
	{ key: "supportsDeveloperRole", kind: "boolean" },
	{ key: "supportsReasoningEffort", kind: "boolean" },
	{ key: "supportsUsageInStreaming", kind: "boolean" },
	{ key: "requiresToolResultName", kind: "boolean" },
	{ key: "requiresAssistantAfterToolResult", kind: "boolean" },
	{ key: "requiresThinkingAsText", kind: "boolean" },
	{ key: "requiresReasoningContentOnAssistantMessages", kind: "boolean" },
	{ key: "supportsStrictMode", kind: "boolean" },
	{ key: "sendSessionAffinityHeaders", kind: "boolean" },
	{ key: "supportsLongCacheRetention", kind: "boolean" },
	{ key: "supportsToolSearch", kind: "boolean" },
	{ key: "supportsEagerToolInputStreaming", kind: "boolean" },
	{ key: "supportsCacheControlOnTools", kind: "boolean" },
	{ key: "forceAdaptiveThinking", kind: "boolean" },
	{ key: "supportsToolReferences", kind: "boolean" },
	{ key: "maxTokensField", kind: "enum", values: ["max_completion_tokens", "max_tokens"] },
	{
		key: "thinkingFormat",
		kind: "enum",
		values: [
			"openai",
			"openrouter",
			"together",
			"deepseek",
			"zai",
			"qwen",
			"chat-template",
			"qwen-chat-template",
			"string-thinking",
			"ant-ling",
		],
	},
	{ key: "cacheControlFormat", kind: "enum", values: ["anthropic"] },
	{ key: "sessionAffinityFormat", kind: "enum", values: ["openai", "openai-nosession", "openrouter"] },
	{ key: "openRouterRouting.allow_fallbacks", kind: "boolean" },
	{ key: "openRouterRouting.require_parameters", kind: "boolean" },
	{ key: "openRouterRouting.zdr", kind: "boolean" },
	{ key: "openRouterRouting.enforce_distillable_text", kind: "boolean" },
	{ key: "openRouterRouting.data_collection", kind: "enum", values: ["deny", "allow"] },
	{ key: "openRouterRouting.order", kind: "list" },
	{ key: "openRouterRouting.only", kind: "list" },
	{ key: "openRouterRouting.ignore", kind: "list" },
	{ key: "openRouterRouting.quantizations", kind: "list" },
	{ key: "openRouterRouting.sort.by", kind: "stringOrNull" },
	{ key: "openRouterRouting.sort.partition", kind: "stringOrNull" },
	{ key: "openRouterRouting.max_price.prompt", kind: "numberOrString" },
	{ key: "openRouterRouting.max_price.completion", kind: "numberOrString" },
	{ key: "openRouterRouting.max_price.image", kind: "numberOrString" },
	{ key: "openRouterRouting.max_price.audio", kind: "numberOrString" },
	{ key: "openRouterRouting.max_price.request", kind: "numberOrString" },
	{ key: "openRouterRouting.preferred_min_throughput", kind: "number" },
	{ key: "openRouterRouting.preferred_max_latency", kind: "number" },
	{ key: "openRouterRouting.preferred_min_throughput.p50", kind: "number" },
	{ key: "openRouterRouting.preferred_min_throughput.p75", kind: "number" },
	{ key: "openRouterRouting.preferred_min_throughput.p90", kind: "number" },
	{ key: "openRouterRouting.preferred_min_throughput.p99", kind: "number" },
	{ key: "openRouterRouting.preferred_max_latency.p50", kind: "number" },
	{ key: "openRouterRouting.preferred_max_latency.p75", kind: "number" },
	{ key: "openRouterRouting.preferred_max_latency.p90", kind: "number" },
	{ key: "openRouterRouting.preferred_max_latency.p99", kind: "number" },
	{ key: "vercelGatewayRouting.only", kind: "list" },
	{ key: "vercelGatewayRouting.order", kind: "list" },
];

export interface SettingsCenterSaveRequest {
	global: Settings;
	project: Settings;
	models: ModelsJson;
	credentials: ProviderCredentialMutation[];
}

export interface SettingsCenterOptions {
	global: Settings;
	project: Settings;
	projectTrusted: boolean;
	models: ModelsJson;
	/** Secret-free status for Providers registered by extensions. Function config is read-only. */
	extensionProviders?: readonly ExtensionProviderRuntimeStatus[];
	providerAuthentication?: Readonly<Record<string, ProviderAuthenticationStatus>>;
	/** Runtime-derived, credential-free Provider templates. */
	providerPresets?: readonly ProviderPreset[];
	resources?: {
		settingsManager: SettingsManager;
		resolvedPaths: ScopedResolvedPaths;
		cwd: string;
		agentDir: string;
		terminalRows: number;
	};
	onSave: (request: SettingsCenterSaveRequest) => Promise<void>;
	onCancel: () => void;
	onStartOAuth: (providerId: string) => void;
	/** Explicit, non-persistent model discovery using the Provider form draft. */
	onDiscoverModels?: (
		draft: ProviderFormDraft,
		stagedApiKey: string | undefined,
	) => Promise<readonly ModelsJsonModel[]>;
}

const PAGES: readonly Page[] = [
	{ id: "providers", label: "Providers & Models" },
	{ id: "defaults", label: "Defaults & Sessions" },
	{ id: "delivery", label: "Context & Delivery" },
	{ id: "appearance", label: "Appearance & Terminal" },
	{ id: "tools", label: "Tools, Shell & Network" },
	{ id: "resources", label: "Resources" },
	{ id: "security", label: "Security & Data" },
];

function boolValue(value: boolean | undefined): string {
	return value ? "On" : "Off";
}

function cycleBoolean(value: boolean | undefined): boolean {
	return !value;
}

/**
 * Centered, transactional settings modal. It keeps every mutable value in this
 * component until Ctrl+S.
 */
export class SettingsCenterComponent implements Component, Focusable {
	private readonly options: SettingsCenterOptions;
	private globalDraft: Settings;
	private projectDraft: Settings;
	private modelsDraft: ModelsJson;
	private scope: SettingsScope = "global";
	private focus: SettingsFocus = "navigation";
	private footerAction: "cancel" | "save" = "save";
	private pageIndex = 0;
	private rowIndex = 0;
	private readonly credentialMutations = new Map<string, Credential | undefined>();
	private credentialProviderId: string | undefined;
	private editorStep: EditorStep | undefined;
	private providerEditor: { id: string; name: string; baseUrl: string; api: string } | undefined;
	private modelEditor:
		| { providerId: string; id: string; name: string; api: string; contextWindow: string; maxTokens: string }
		| undefined;
	private editTarget: { providerId: string; modelId?: string } | undefined;
	private headerEditor: { providerId: string; modelId?: string; overrideId?: string; name: string } | undefined;
	private advancedTarget: { providerId: string; modelId: string; overrideId?: string } | undefined;
	private costEditor: CostEditorDraft | undefined;
	private tierEditor: CostTierEditorDraft | undefined;
	private compatTarget:
		| { providerId: string; modelId?: string; overrideId?: string; field?: CompatField; templateName?: string }
		| undefined;
	private overrideTarget: { providerId: string; id: string } | undefined;
	private settingEditor:
		| { label: string; apply: (value: string) => string | undefined; preserveWhitespace?: boolean }
		| undefined;
	private modelDeletionPicker: { targets: ModelDeletionTarget[]; index: number; confirming: boolean } | undefined;
	private actionMenu: SettingsActionMenu | undefined;
	private editorModelsBefore: ModelsJson | undefined;
	private input: Input | undefined;
	private status = "Draft changes are not saved";
	private saving = false;
	private bodyScroll = 0;
	private bodyViewportRows = 1;
	private manualBodyScroll = false;
	private readonly resourceList: ResourceSettingsList | undefined;
	private readonly providersBrowser = new ModelsProvidersBrowser();
	private providerForm: ProviderFormComponent | undefined;
	private providerFormOriginalId: string | undefined;
	private providerDetail: ProviderSettingsDetail | undefined;
	private modelDetail: ModelSettingsDetail | undefined;
	private readonly shell: ModalShell;
	focused = false;

	constructor(options: SettingsCenterOptions) {
		this.options = options;
		this.globalDraft = structuredClone(options.global);
		this.projectDraft = structuredClone(options.project);
		this.modelsDraft = structuredClone(options.models);
		this.providersBrowser.onActivate = (entry) => this.openModelsProvidersEntry(entry);
		if (options.resources) {
			this.resourceList = createResourceSettingsList(
				options.resources.resolvedPaths,
				options.resources.settingsManager,
				options.resources.cwd,
				options.resources.agentDir,
				options.resources.terminalRows,
				"global",
			);
		}
		this.shell = new ModalShell({
			title: () => theme.bold(theme.fg("text", "Settings")),
			renderHeader: (width) => this.renderModalHeader(width),
			renderBody: (width, height) => this.renderModalBody(width, height),
			renderFooter: (width) => this.renderFooter(width),
		});
	}

	invalidate(): void {}

	render(width: number): string[] {
		return this.shell.render(width);
	}

	setViewportRows(rows: number): void {
		this.shell.setViewportRows(rows);
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (this.actionMenu) {
			this.handleActionMenuInput(data, keybindings);
			return;
		}
		if (this.modelDeletionPicker) {
			this.handleModelDeletionPickerInput(data, keybindings);
			return;
		}
		if (this.editorStep && this.input) {
			this.input.handleInput(data);
			return;
		}
		if (keybindings.matches(data, "app.settings.save")) {
			void this.save();
			return;
		}
		if (keybindings.matches(data, "tui.select.cancel")) {
			if (this.providerForm) {
				if (!this.providerForm.closeTransientState()) this.closeProviderForm();
				return;
			}
			// Provider and Model details form a drill-down stack inside the
			// Settings content. Esc returns one level before it can close the
			// Settings overlay itself.
			if (this.modelDetail) {
				this.modelDetail.handleInput(data);
				return;
			}
			if (this.providerDetail) {
				this.providerDetail.handleInput(data);
				return;
			}
			this.options.onCancel();
			return;
		}
		// Tab remains a quiet compatibility alias for existing terminal users. The
		// visible and documented focus protocol is Shift+arrow in every TUI view.
		if (keybindings.matches(data, "tui.input.tab")) {
			this.focus = this.focus === "content" ? "navigation" : "content";
			this.bodyScroll = 0;
			this.manualBodyScroll = false;
			return;
		}
		if (this.moveFocus(data, keybindings)) {
			return;
		}
		if (this.focus === "scope") {
			if (keybindings.matches(data, "tui.select.confirm")) {
				this.toggleScope();
			}
			return;
		}
		if (this.focus === "actions") {
			if (keybindings.matches(data, "tui.select.up")) {
				this.footerAction = "cancel";
				return;
			}
			if (keybindings.matches(data, "tui.select.down")) {
				this.footerAction = "save";
				return;
			}
			if (keybindings.matches(data, "tui.select.confirm")) {
				if (this.footerAction === "save") void this.save();
				else this.options.onCancel();
			}
			return;
		}
		if (this.focus === "navigation") {
			if (keybindings.matches(data, "tui.select.up")) {
				this.pageIndex = (this.pageIndex + PAGES.length - 1) % PAGES.length;
				this.bodyScroll = 0;
				this.manualBodyScroll = false;
			}
			if (keybindings.matches(data, "tui.select.down")) {
				this.pageIndex = (this.pageIndex + 1) % PAGES.length;
				this.bodyScroll = 0;
				this.manualBodyScroll = false;
			}
			if (keybindings.matches(data, "tui.select.confirm")) {
				this.focus = "content";
				this.manualBodyScroll = false;
			}
			return;
		}
		this.handleContentInput(data);
	}

	private moveFocus(data: string, keybindings: ReturnType<typeof getKeybindings>): boolean {
		const left = keybindings.matches(data, "app.focus.left");
		const right = keybindings.matches(data, "app.focus.right");
		const up = keybindings.matches(data, "app.focus.up");
		const down = keybindings.matches(data, "app.focus.down");
		if (!left && !right && !up && !down) return false;

		const previous = this.focus;
		switch (this.focus) {
			case "scope":
				if (left || right) this.toggleScope();
				else if (down) this.focus = "navigation";
				break;
			case "navigation":
				if (up) this.focus = "scope";
				else if (right) this.focus = "content";
				break;
			case "content":
				if (left) this.focus = "navigation";
				else if (up) this.focus = "scope";
				else if (down) this.focus = "actions";
				break;
			case "actions":
				if (up) this.focus = "content";
				else if (left || right) this.footerAction = left ? "cancel" : "save";
				break;
		}
		if (previous !== this.focus) {
			this.bodyScroll = 0;
			this.manualBodyScroll = false;
		}
		return true;
	}

	private renderModalHeader(contentWidth: number): string[] {
		const selectedScope = this.scope === "global" ? "Global" : "Project";
		const scopeText = this.scope === "global" ? theme.bold(theme.fg("accent", "Global")) : "Global";
		const projectText = this.scope === "project" ? theme.bold(theme.fg("accent", "Project")) : "Project";
		const scope = `${this.focus === "scope" ? theme.fg("accent", "› ") : "  "}${scopeText} / ${projectText}`;
		const state = this.saving ? theme.fg("warning", "Saving...") : theme.fg("muted", `${selectedScope} scope`);
		const lines = [alignEnd(scope, state, contentWidth)];
		if (this.scope === "project" && !this.options.projectTrusted) {
			lines.push(theme.fg("warning", "Project is untrusted; this scope is read-only."));
		}
		return lines;
	}

	private renderNavigation(width: number): string[] {
		const lines = [theme.fg("muted", this.focus === "navigation" ? "Pages · Focus" : "Pages"), ""];
		for (let index = 0; index < PAGES.length; index++) {
			const page = PAGES[index]!;
			const selected = index === this.pageIndex;
			const prefix = selected ? theme.fg("accent", "› ") : "  ";
			const text = selected ? theme.bold(theme.fg("accent", page.label)) : theme.fg("muted", page.label);
			lines.push(truncateToWidth(`${prefix}${text}`, width, ""));
		}
		return lines;
	}

	private renderModalBody(width: number, height: number): string[] {
		this.bodyViewportRows = Math.max(1, height);
		if (this.actionMenu) return this.renderViewport(this.renderActionMenu(width), height, this.actionMenu.index + 2);
		if (this.modelDeletionPicker)
			return this.renderViewport(this.renderModelDeletionPicker(width), height, this.modelDeletionPicker.index + 2);
		if (this.editorStep && this.input) return this.renderViewport(this.renderEditor(width), height, 2);

		const compact = width < 64;
		if (compact) {
			if (this.focus === "navigation") return this.renderViewport(this.renderNavigation(width), height);
			return this.renderViewport(this.renderContent(width), height, this.contentSelectedLine());
		}

		const separator = " │ ";
		const navigationWidth = Math.min(26, Math.max(20, Math.floor((width - 3) * 0.29)));
		const contentWidth = Math.max(20, width - navigationWidth - 3);
		const navigation = this.renderNavigation(navigationWidth);
		const content = this.renderViewport(this.renderContent(contentWidth), height, this.contentSelectedLine());
		return Array.from({ length: height }, (_, index) =>
			joinColumns(navigation[index] ?? "", navigationWidth, separator, content[index] ?? "", contentWidth),
		);
	}

	private renderViewport(lines: string[], height: number, selectedLine?: number): string[] {
		const viewportRows = Math.max(1, height);
		const maximumOffset = Math.max(0, lines.length - viewportRows);
		if (!this.manualBodyScroll && selectedLine !== undefined) {
			if (selectedLine < this.bodyScroll) this.bodyScroll = selectedLine;
			if (selectedLine >= this.bodyScroll + viewportRows) this.bodyScroll = selectedLine - viewportRows + 1;
		}
		this.bodyScroll = Math.max(0, Math.min(this.bodyScroll, maximumOffset));
		return lines.slice(this.bodyScroll, this.bodyScroll + viewportRows);
	}

	private contentSelectedLine(): number | undefined {
		if (this.editorStep) return 2;
		const page = PAGES[this.pageIndex]!;
		if (page.id === "providers") {
			if (this.providerForm) return 2 + this.providerForm.selectedLine();
			if (this.modelDetail) return 2 + this.modelDetail.selectedLine();
			if (this.providerDetail) return 2 + this.providerDetail.selectedLine();
			return this.providersBrowser.selectedLine() === undefined
				? undefined
				: 2 + this.providersBrowser.selectedLine()!;
		}
		if (page.id === "resources") return undefined;
		return 2 + this.rowIndex;
	}

	private renderContent(width: number): string[] {
		if (this.editorStep && this.input) return this.renderEditor(width);
		const page = PAGES[this.pageIndex]!;
		const title = theme.bold(theme.fg("text", `${this.focus === "content" ? "Focus · " : ""}${page.label}`));
		switch (page.id) {
			case "providers":
				return [title, "", ...this.renderProviders(width)];
			case "defaults":
				return [title, "", ...this.renderRows(width, this.defaultRows())];
			case "delivery":
				return [title, "", ...this.renderRows(width, this.deliveryRows())];
			case "appearance":
				return [title, "", ...this.renderRows(width, this.appearanceRows())];
			case "tools":
				return [title, "", ...this.renderRows(width, this.toolsRows())];
			case "resources":
				return this.renderResources(title, width);
			case "security":
				return [title, "", ...this.renderRows(width, this.securityRows())];
		}
	}

	private renderFooter(width: number): string[] {
		const actions =
			this.focus === "actions"
				? `${this.footerAction === "cancel" ? theme.fg("accent", "› Cancel") : "  Cancel"}  ${this.footerAction === "save" ? theme.fg("accent", "› Save") : "  Save"}`
				: "Cancel · Save";
		// The footer is part of the fixed modal frame. Keep the two universally
		// useful commands at the left edge on narrow terminals instead of letting
		// a long transient status hide them behind truncation.
		const controls =
			width < 58
				? `Ctrl+S · Esc · Shift focus · ${actions}`
				: width < 90
					? `Ctrl+S save · Esc cancel · Shift focus · ${actions}`
					: `Ctrl+S save · Esc cancel · Shift+arrows focus · Enter open · ${actions}`;
		// The validation state and commands are independent, equally important
		// information. A two-row fixed footer gives each a stable visual slot and
		// prevents either one from disappearing on small terminals.
		return [
			theme.fg("muted", truncateToWidth(this.status, width, "")),
			theme.fg("muted", truncateToWidth(controls, width, "")),
		];
	}

	private renderProviders(width: number): string[] {
		if (this.providerForm) return this.providerForm.render(width);
		if (this.modelDetail) {
			const provider = this.modelsDraft.providers[this.modelDetail.getProviderId()];
			const model = provider?.models?.find((candidate) => candidate.id === this.modelDetail!.getModelId());
			if (provider && model) this.modelDetail.setModel(provider, model);
			return this.modelDetail.render(width);
		}
		if (this.providerDetail) {
			const provider = this.modelsDraft.providers[this.providerDetail.getProviderId()];
			if (provider)
				this.providerDetail.setProvider(provider, this.authenticationFor(this.providerDetail.getProviderId()));
			return this.providerDetail.render(width);
		}

		this.syncProvidersBrowser();
		const extensionProviders = [...(this.options.extensionProviders ?? [])].sort((left, right) =>
			left.providerId.localeCompare(right.providerId),
		);
		const lines = this.providersBrowser.render(width);
		if (extensionProviders.length > 0) {
			lines.push("", theme.fg("muted", "Extension runtime providers (read-only function configuration)"));
			for (const provider of extensionProviders) {
				const auth = provider.auth.configured
					? `configured${provider.auth.source ? ` · ${provider.auth.source}` : ""}${provider.auth.label ? ` (${provider.auth.label})` : ""}`
					: "not configured";
				const capabilities = [
					provider.capabilities.oauth ? "OAuth" : undefined,
					provider.capabilities.customStream ? "custom stream" : undefined,
					provider.capabilities.dynamicModels ? "dynamic models" : undefined,
				]
					.filter((value): value is string => value !== undefined)
					.join(", ");
				lines.push(
					truncateToWidth(
						`${theme.fg("accent", "  extension ")}${provider.providerId} · ${provider.configuration.replaceAll("+", " + ")}`,
						width,
						"",
					),
				);
				lines.push(
					truncateToWidth(
						theme.fg("dim", `      source ${provider.sourcePath ?? "runtime registration"} · auth ${auth}`),
						width,
						"",
					),
				);
				lines.push(
					truncateToWidth(
						theme.fg(
							"dim",
							`      models ${provider.availableModelCount}/${provider.modelCount} available · ${capabilities || "static model configuration"}`,
						),
						width,
						"",
					),
				);
				if (provider.compositionError) {
					lines.push(
						truncateToWidth(
							theme.fg("error", `      composition error: ${provider.compositionError}`),
							width,
							"",
						),
					);
				}
			}
		}
		lines.push(
			"",
			theme.fg(
				"muted",
				"Provider defines connection and auth; each Model is its remote model ID. Draft changes save with Ctrl+S.",
			),
		);
		return lines;
	}

	private syncProvidersBrowser(): void {
		this.providersBrowser.setData(this.modelsDraft.providers);
	}

	private openModelsProvidersEntry(entry: ModelsProvidersEntry): void {
		switch (entry.kind) {
			case "provider":
				this.openProviderForm(entry.providerId);
				return;
			case "add-provider":
				this.openProviderForm();
				return;
		}
	}

	private openProviderForm(providerId?: string): void {
		if (!this.requireGlobalModelConfiguration()) return;
		const provider = providerId ? this.modelsDraft.providers[providerId] : undefined;
		if (providerId && !provider) {
			this.status = "Provider no longer exists in this draft";
			return;
		}
		this.providerDetail = undefined;
		this.modelDetail = undefined;
		this.providerFormOriginalId = providerId;
		const form = new ProviderFormComponent({
			mode: provider ? "edit" : "create",
			...(provider ? { draft: { id: providerId!, provider } } : {}),
			presets: this.options.providerPresets ?? [],
			authentication: providerId ? this.authenticationFor(providerId) : { oauthAvailable: false },
			getAuthentication: (id) => this.authenticationFor(id),
			callbacks: {
				onDraftChange: (draft) => this.applyProviderFormDraft(draft),
				onStageApiKey: (id, apiKey) => {
					if (!id) {
						this.status = "Set the Provider ID before entering an API key";
						return;
					}
					this.credentialMutations.set(id, { type: "api_key", key: apiKey });
					this.status = "Provider API key added to draft";
				},
				onStageOAuth: (id) => {
					if (!id) {
						this.status = "Set the Provider ID before starting OAuth";
						return;
					}
					this.options.onStartOAuth(id);
				},
				onClearAuthentication: (id) => this.confirmProviderCredentialRemoval(id),
				onFetchModels: async (draft, stagedApiKey) =>
					this.options.onDiscoverModels ? this.options.onDiscoverModels(draft, stagedApiKey) : [],
			},
		});
		form.focused = this.focused;
		this.providerForm = form;
		this.bodyScroll = 0;
		this.manualBodyScroll = false;
		this.status = provider ? `Editing Provider · ${providerId}` : "Choose a template or configure a custom Provider";
	}

	private applyProviderFormDraft(draft: ProviderFormDraft): void {
		if (!draft.id) return;
		if (this.providerFormOriginalId && this.providerFormOriginalId !== draft.id) {
			delete this.modelsDraft.providers[this.providerFormOriginalId];
			this.credentialMutations.delete(this.providerFormOriginalId);
		}
		this.modelsDraft.providers[draft.id] = structuredClone(draft.provider);
		this.providerFormOriginalId = draft.id;
		this.status = `Provider ${draft.id} updated in draft`;
	}

	private confirmProviderCredentialRemoval(providerId: string): void {
		if (!providerId) {
			this.status = "Set the Provider ID before clearing authentication";
			return;
		}
		this.openActionMenu("Clear Provider authentication", providerId, [
			{
				label: "Clear authentication",
				action: () => {
					this.credentialMutations.set(providerId, undefined);
					this.status = "Authentication removal added to draft";
				},
			},
			{ label: "Cancel", action: () => this.statusMessage("Authentication change cancelled") },
		]);
	}

	private closeProviderForm(): void {
		this.providerForm = undefined;
		this.providerFormOriginalId = undefined;
		this.bodyScroll = 0;
		this.manualBodyScroll = false;
		this.status = "Back to Providers";
	}

	private openProviderDetail(providerId: string): void {
		const provider = this.modelsDraft.providers[providerId];
		if (!provider) {
			this.status = "Provider no longer exists in this draft";
			return;
		}
		const detail = new ProviderSettingsDetail(providerId, provider, this.authenticationFor(providerId));
		detail.onActivate = (item) => this.activateProviderDetailItem(providerId, item);
		detail.onBack = () => {
			this.providerDetail = undefined;
			this.status = "Back to Providers";
		};
		this.providerDetail = detail;
		this.bodyScroll = 0;
		this.manualBodyScroll = false;
		this.status = `Editing Provider · ${providerId}`;
	}

	private authenticationFor(providerId: string): ProviderAuthenticationStatus {
		const staged = this.credentialMutations.get(providerId);
		if (this.credentialMutations.has(providerId)) {
			return {
				type: staged?.type,
				oauthAvailable: this.options.providerAuthentication?.[providerId]?.oauthAvailable ?? true,
			};
		}
		return this.options.providerAuthentication?.[providerId] ?? { oauthAvailable: true };
	}

	private activateProviderDetailItem(providerId: string, item: ProviderDetailItem): void {
		if (!this.requireGlobalModelConfiguration()) return;
		const provider = this.modelsDraft.providers[providerId];
		if (!provider) return;
		switch (item.kind) {
			case "provider-name":
				this.editProviderField(providerId, "Display name (empty clears)", "name");
				return;
			case "provider-base-url":
				this.editProviderField(providerId, "Base URL (empty clears)", "baseUrl");
				return;
			case "provider-api":
				this.editProviderField(providerId, "API protocol (empty clears)", "api");
				return;
			case "provider-auth-header":
				this.editSetting(
					"Authorization header: auto, on, or off",
					provider.authHeader === undefined ? "auto" : provider.authHeader ? "on" : "off",
					(value) =>
						this.setProviderBooleanField(providerId, "authHeader", value) ? undefined : "Use auto, on, or off",
				);
				return;
			case "provider-oauth":
				this.editSetting("OAuth type: radius or off", provider.oauth ?? "off", (value) =>
					this.setProviderOauth(providerId, value) ? undefined : "Use radius or off",
				);
				return;
			case "provider-api-key":
				this.startProviderApiKeyEditor(providerId);
				return;
			case "provider-oauth-login":
				if (!this.authenticationFor(providerId).oauthAvailable) {
					this.status = "This Provider does not offer OAuth login";
					return;
				}
				this.options.onStartOAuth(providerId);
				return;
			case "provider-auth-clear":
				this.startProviderCredentialRemoval(providerId);
				return;
			case "provider-headers":
				this.startHeaderEditorFor(providerId);
				return;
			case "provider-compat":
				this.startCompatEditorFor({ providerId });
				return;
			case "model":
				this.openModelDetail(providerId, item.modelId);
				return;
			case "add-model":
				this.startModelEditorFor(providerId);
				return;
			case "override":
				this.startOverrideEditorFor(providerId, item.overrideId);
				return;
			case "add-override":
				this.startOverrideEditorFor(providerId);
				return;
			case "delete-provider":
				this.startModelDeletionFor({ kind: "provider", providerId });
		}
	}

	private openModelDetail(providerId: string, modelId: string): void {
		const provider = this.modelsDraft.providers[providerId];
		const model = provider?.models?.find((candidate) => candidate.id === modelId);
		if (!provider || !model) {
			this.status = "Model no longer exists in this draft";
			return;
		}
		const detail = new ModelSettingsDetail(providerId, provider, model);
		detail.onActivate = (item) => this.activateModelDetailItem(providerId, modelId, item);
		detail.onBack = () => {
			this.modelDetail = undefined;
			this.status = `Back to Provider · ${providerId}`;
		};
		this.modelDetail = detail;
		this.bodyScroll = 0;
		this.manualBodyScroll = false;
		this.status = `Editing remote model · ${modelId}`;
	}

	private activateModelDetailItem(providerId: string, modelId: string, item: ModelDetailItem): void {
		if (!this.requireGlobalModelConfiguration()) return;
		const model = this.modelsDraft.providers[providerId]?.models?.find((candidate) => candidate.id === modelId);
		if (!model) return;
		switch (item.kind) {
			case "model-name":
				this.editModelTextField(providerId, modelId, "Display name (empty clears)", "name");
				return;
			case "model-api":
				this.editModelTextField(providerId, modelId, "API protocol override (empty inherits Provider)", "api");
				return;
			case "model-base-url":
				this.editModelTextField(providerId, modelId, "Base URL override (empty inherits Provider)", "baseUrl");
				return;
			case "model-headers":
				this.startHeaderEditorFor(providerId, modelId);
				return;
			case "model-compat":
				this.startCompatEditorFor({ providerId, modelId });
				return;
			case "model-reasoning":
				this.editSetting(
					"Reasoning support: auto, on, or off",
					model.reasoning === undefined ? "auto" : model.reasoning ? "on" : "off",
					(value) =>
						this.setModelBooleanField(providerId, modelId, "reasoning", value)
							? undefined
							: "Use auto, on, or off",
				);
				return;
			case "model-input":
				this.editSetting(
					"Input modalities: text or text,image (empty inherits Provider)",
					model.input?.join(",") ?? "",
					(value) => (this.setModelInputField(providerId, modelId, value) ? undefined : "Use text or text,image"),
				);
				return;
			case "model-context-window":
				this.editPositiveInteger("Context window tokens (empty clears)", model.contextWindow, (value) => {
					this.setModelNumberField(providerId, modelId, "contextWindow", value);
				});
				return;
			case "model-max-tokens":
				this.editPositiveInteger("Maximum output tokens (empty clears)", model.maxTokens, (value) => {
					this.setModelNumberField(providerId, modelId, "maxTokens", value);
				});
				return;
			case "model-thinking-cost":
				this.startModelAdvancedEditorFor(providerId, modelId);
				return;
			case "delete-model":
				this.startModelDeletionFor({ kind: "model", providerId, modelId });
		}
	}

	private requireGlobalModelConfiguration(): boolean {
		if (this.scope === "global") return true;
		this.status = "Provider and Model definitions are global; switch to Global scope to edit";
		return false;
	}

	private editProviderField(providerId: string, label: string, field: "name" | "baseUrl" | "api"): void {
		const current = this.modelsDraft.providers[providerId]?.[field] ?? "";
		this.editSetting(label, current, (value) => {
			this.setProviderTextField(providerId, field, value);
			return undefined;
		});
	}

	private editModelTextField(
		providerId: string,
		modelId: string,
		label: string,
		field: "name" | "api" | "baseUrl",
	): void {
		const current = this.findDraftModel(providerId, modelId)[field] ?? "";
		this.editSetting(label, current, (value) => {
			this.setModelTextField(providerId, modelId, field, value);
			return undefined;
		});
	}

	private editPositiveInteger(label: string, current: number | undefined, apply: (value: string) => void): void {
		this.editSetting(label, current === undefined ? "" : String(current), (value) => {
			if (value && (!Number.isSafeInteger(Number(value)) || Number(value) <= 0)) return "Enter a positive integer";
			apply(value);
			return undefined;
		});
	}

	private startModelEditorFor(providerId: string): void {
		if (!this.requireGlobalModelConfiguration()) return;
		this.modelEditor = { providerId, id: "", name: "", api: "", contextWindow: "", maxTokens: "" };
		this.openEditor("modelId");
	}

	private startProviderApiKeyEditor(providerId: string): void {
		if (!this.requireGlobalModelConfiguration()) return;
		const openEditor = () => {
			this.credentialProviderId = providerId;
			this.openEditor("providerCredentialApiKey");
		};
		if (!this.authenticationFor(providerId).type) {
			openEditor();
			return;
		}
		this.openActionMenu("Replace Provider authentication", providerId, [
			{ label: "Replace with API Key", action: openEditor },
			{ label: "Cancel", action: () => this.statusMessage("Authentication change cancelled") },
		]);
	}

	private startProviderCredentialRemoval(providerId: string): void {
		if (!this.requireGlobalModelConfiguration()) return;
		if (!this.authenticationFor(providerId).type) {
			this.status = "This Provider has no stored authentication";
			return;
		}
		this.openActionMenu("Clear Provider authentication", providerId, [
			{
				label: "Clear authentication",
				action: () => {
					this.credentialMutations.set(providerId, undefined);
					this.status = "Authentication removal added to draft";
				},
			},
			{ label: "Cancel", action: () => this.statusMessage("Authentication change cancelled") },
		]);
	}

	private startHeaderEditorFor(providerId: string, modelId?: string): void {
		this.headerEditor = { providerId, ...(modelId ? { modelId } : {}), name: "" };
		this.openEditor("headerName");
	}

	private startCompatEditorFor(target: { providerId: string; modelId?: string }): void {
		this.compatTarget = { ...target };
		this.openEditor("compatField");
	}

	private startModelAdvancedEditorFor(providerId: string, modelId: string): void {
		this.advancedTarget = { providerId, modelId };
		this.openEditor("advancedThinkingOff");
		this.beginThinkingEditor();
	}

	private startOverrideEditorFor(providerId: string, overrideId?: string): void {
		this.overrideTarget = { providerId, id: overrideId ?? "" };
		this.openEditor(overrideId ? "overrideName" : "overrideId");
		if (overrideId) this.overrideConfig();
	}

	private startModelDeletionFor(target: ModelDeletionTarget): void {
		this.modelDeletionPicker = { targets: [target], index: 0, confirming: false };
		this.status = "Confirm deletion of the selected configuration";
	}

	private renderRows(width: number, rows: readonly SettingsRow[]): string[] {
		return rows.map((row, index) => {
			const selected = index === this.rowIndex;
			const prefix = selected ? theme.fg("accent", "› ") : "  ";
			const label = selected ? theme.fg("accent", row.label) : row.label;
			return truncateToWidth(`${prefix}${label}  ${theme.fg("muted", row.value)}`, width, "");
		});
	}

	private renderEditor(width: number): string[] {
		const promptByStep: Record<EditorStep, string> = {
			providerId: "Provider ID",
			providerName: "Provider display name",
			providerBaseUrl: "Base URL (optional; Enter to skip)",
			providerApi: "API type (optional; Enter to skip)",
			modelProvider: "Provider ID for this model",
			modelId: "Model ID",
			modelName: "Model display name (optional; Enter to skip)",
			modelApi: "Model API override (optional; Enter to skip)",
			modelContext: "Context window tokens (optional; Enter to skip)",
			modelMaxTokens: "Maximum output tokens (optional; Enter to skip)",
			editProvider: "Existing Provider ID",
			editModelId: "Model ID (optional; Enter to edit provider)",
			editProviderName: "Provider display name (empty clears)",
			editProviderBaseUrl: "Provider Base URL (empty clears)",
			editProviderApi: "Provider API type (empty clears)",
			editProviderAuthHeader: "Automatic Authorization header: auto, on, or off (empty clears)",
			editProviderOauth: "OAuth type: radius or off (empty clears)",
			providerCredentialApiKey: "API key (stored securely in auth.json; Enter saves to this draft)",
			editModelName: "Model display name (empty clears)",
			editModelApi: "Model API override (empty clears)",
			editModelBaseUrl: "Model Base URL override (empty clears)",
			editModelReasoning: "Reasoning support: auto, on, or off (empty clears)",
			editModelInput: "Input modalities: text or text,image (empty clears)",
			editModelContext: "Context window tokens (empty clears)",
			editModelMaxTokens: "Maximum output tokens (empty clears)",
			headerProvider: "Provider ID for headers",
			headerModelId: "Model ID (optional; Enter for provider headers)",
			headerName: "Header name",
			headerValue: "Header value (empty removes this header)",
			advancedProvider: "Existing Provider ID",
			advancedModelId: "Model ID",
			advancedThinkingOff: "Thinking map: off effort (empty inherits; - disables thinking)",
			advancedThinkingMinimal: "Thinking map: minimal effort (empty inherits; - disables thinking)",
			advancedThinkingLow: "Thinking map: low effort (empty inherits; - disables thinking)",
			advancedThinkingMedium: "Thinking map: medium effort (empty inherits; - disables thinking)",
			advancedThinkingHigh: "Thinking map: high effort (empty inherits; - disables thinking)",
			advancedThinkingXhigh: "Thinking map: xhigh effort (empty inherits; - disables thinking)",
			advancedThinkingMax: "Thinking map: max effort (empty inherits; - disables thinking)",
			advancedCostInput: "Input cost per 1M tokens (number, or clear to remove all cost settings)",
			advancedCostOutput: "Output cost per 1M tokens (number)",
			advancedCostCacheRead: "Cache read cost per 1M tokens (number)",
			advancedCostCacheWrite: "Cache write cost per 1M tokens (number)",
			advancedTierAction: "Cost tiers: add, remove N, clear, or done",
			advancedTierThreshold: "Tier input tokens above (integer)",
			advancedTierInput: "Tier input cost per 1M tokens (number)",
			advancedTierOutput: "Tier output cost per 1M tokens (number)",
			advancedTierCacheRead: "Tier cache read cost per 1M tokens (number)",
			advancedTierCacheWrite: "Tier cache write cost per 1M tokens (number)",
			compatProvider: "Existing Provider ID",
			compatModelId: "Model ID (optional; Enter for provider compatibility)",
			compatField:
				"Compatibility field (type clear FIELD to remove; chatTemplateKwargs for a named template argument)",
			compatTemplateName: "Chat template kwarg name",
			compatValue: "Compatibility value (empty clears this field)",
			settingValue: this.settingEditor?.label ?? "Setting value",
			overrideProvider: "Existing Provider ID",
			overrideId: "Model ID or override pattern",
			overrideName: "Override display name (empty clears)",
			overrideReasoning: "Override reasoning support: auto, on, or off (empty clears)",
			overrideInput: "Override input modalities: text or text,image (empty clears)",
			overrideContext: "Override context window tokens (empty clears)",
			overrideMaxTokens: "Override maximum output tokens (empty clears)",
		};
		const title = this.settingEditor
			? "Edit Setting"
			: this.credentialProviderId
				? "Provider API Key"
				: this.providerEditor
					? "New Provider"
					: this.modelEditor
						? "New Model"
						: this.headerEditor
							? "Edit Headers"
							: this.overrideTarget || this.editorStep === "overrideProvider"
								? "Model Override"
								: this.compatTarget || this.editorStep === "compatProvider"
									? "Compatibility"
									: this.advancedTarget || this.editorStep === "advancedProvider"
										? "Model Behavior & Cost"
										: "Edit Provider or Model";
		const advancedSummary = this.advancedTarget?.modelId ? this.renderAdvancedSummary(width) : [];
		const compatSummary = this.compatTarget ? this.renderCompatSummary(width) : [];
		return [
			theme.bold(theme.fg("text", title)),
			"",
			...advancedSummary,
			...compatSummary,
			promptByStep[this.editorStep!],
			...this.input!.render(width),
			"",
			theme.fg("muted", "Enter next · Esc cancel"),
		];
	}

	private handleContentInput(data: string): void {
		const keybindings = getKeybindings();
		if (PAGES[this.pageIndex]?.id === "providers" && this.providerForm) {
			this.providerForm.handleInput(data);
			return;
		}
		if (keybindings.matches(data, "tui.select.pageUp")) {
			this.bodyScroll = Math.max(0, this.bodyScroll - Math.max(1, this.bodyViewportRows - 1));
			this.manualBodyScroll = true;
			return;
		}
		if (keybindings.matches(data, "tui.select.pageDown")) {
			this.bodyScroll += Math.max(1, this.bodyViewportRows - 1);
			this.manualBodyScroll = true;
			return;
		}
		if (keybindings.matches(data, "tui.select.up") || keybindings.matches(data, "tui.select.down")) {
			this.manualBodyScroll = false;
		}
		const page = PAGES[this.pageIndex]!;
		if (page.id === "providers") {
			if (this.modelDetail) {
				this.modelDetail.handleInput(data);
				return;
			}
			if (this.providerDetail) {
				this.providerDetail.handleInput(data);
				return;
			}
			if (keybindings.matches(data, "app.settings.provider.new")) this.startProviderEditor();
			else if (keybindings.matches(data, "app.settings.model.new")) this.startModelEditor();
			else if (keybindings.matches(data, "app.settings.models.edit")) this.startModelEditorEdit();
			else if (keybindings.matches(data, "app.settings.headers.edit")) this.startHeaderEditor();
			else if (keybindings.matches(data, "app.settings.model.advanced")) this.startModelAdvancedEditor();
			else if (keybindings.matches(data, "app.settings.compat.edit")) this.startCompatEditor();
			else if (keybindings.matches(data, "app.settings.override.edit")) this.startOverrideEditor();
			else if (keybindings.matches(data, "app.settings.models.delete")) this.startModelDeletionPicker();
			else {
				this.syncProvidersBrowser();
				this.providersBrowser.handleInput(data);
			}
			return;
		}
		if (page.id === "resources") {
			this.resourceList?.handleInput?.(data);
			return;
		}
		const rows = this.rowsForPage(page.id);
		if (keybindings.matches(data, "tui.select.up")) this.rowIndex = Math.max(0, this.rowIndex - 1);
		else if (keybindings.matches(data, "tui.select.down"))
			this.rowIndex = Math.min(Math.max(0, rows.length - 1), this.rowIndex + 1);
		else if (keybindings.matches(data, "tui.select.confirm")) rows[this.rowIndex]?.action();
	}

	private activeDraft(): Settings {
		return this.scope === "global" ? this.globalDraft : this.projectDraft;
	}

	private toggleScope(): void {
		if (this.scope === "global" && !this.options.projectTrusted) {
			this.status = "Project settings are read-only until this project is trusted";
			return;
		}
		this.scope = this.scope === "global" ? "project" : "global";
		this.resourceList?.setWriteScope(this.scope);
		this.rowIndex = 0;
		this.bodyScroll = 0;
		this.manualBodyScroll = false;
		this.status = "Draft changes are not saved";
	}

	private renderResources(title: string, width: number): string[] {
		if (!this.resourceList) {
			return [title, "", theme.fg("warning", "Resource draft could not be initialized.")];
		}
		return [
			title,
			"",
			theme.fg("muted", "Space toggles a resource. Type to filter. Project scope cycles inherit, load, unload."),
			"",
			...this.resourceList.render(width),
		];
	}

	private settingsWithResourceDraft(scope: SettingsScope): Settings {
		const draft = structuredClone(scope === "global" ? this.globalDraft : this.projectDraft);
		const resources =
			scope === "global"
				? this.options.resources?.settingsManager.getGlobalSettings()
				: this.options.resources?.settingsManager.getProjectSettings();
		const draftFields = draft as Record<string, unknown>;
		for (const field of ["skills", "prompts", "themes"] as const) {
			const value = resources?.[field];
			if (value === undefined) delete draftFields[field];
			else draftFields[field] = structuredClone(value);
		}
		return draft;
	}

	private defaultRows(): SettingsRow[] {
		const draft = this.activeDraft();
		return [
			{
				label: "Default provider",
				value: draft.defaultProvider ?? "Not set",
				action: () => this.cycleDefaultModel(),
			},
			{
				label: "Default model",
				value: draft.defaultModel ?? "Not set",
				action: () => this.cycleDefaultModel(),
			},
			{
				label: "Default thinking",
				value: draft.defaultThinkingLevel ?? "Model default",
				action: () => {
					const levels = [undefined, "minimal", "low", "medium", "high"] as const;
					const index = levels.indexOf(draft.defaultThinkingLevel as (typeof levels)[number]);
					draft.defaultThinkingLevel = levels[(index + 1) % levels.length];
				},
			},
		];
	}

	private cycleDefaultModel(): void {
		const available = Object.entries(this.modelsDraft.providers).flatMap(([providerId, provider]) =>
			(provider.models ?? []).map((model) => ({ providerId, modelId: model.id })),
		);
		if (available.length === 0) {
			this.status = "Add a provider model in Providers & Models first";
			return;
		}
		const draft = this.activeDraft();
		const currentIndex = available.findIndex(
			(candidate) => candidate.providerId === draft.defaultProvider && candidate.modelId === draft.defaultModel,
		);
		const next = available[(currentIndex + 1) % available.length]!;
		draft.defaultProvider = next.providerId;
		draft.defaultModel = next.modelId;
	}

	private deliveryRows(): SettingsRow[] {
		const draft = this.activeDraft();
		return [
			{
				label: "Auto compact",
				value: boolValue(draft.compaction?.enabled),
				action: () => this.setCompaction(!draft.compaction?.enabled),
			},
			{
				label: "Steering mode",
				value: draft.steeringMode ?? "one-at-a-time",
				action: () => {
					draft.steeringMode = draft.steeringMode === "all" ? "one-at-a-time" : "all";
				},
			},
			{
				label: "Follow-up mode",
				value: draft.followUpMode ?? "one-at-a-time",
				action: () => {
					draft.followUpMode = draft.followUpMode === "all" ? "one-at-a-time" : "all";
				},
			},
			{
				label: "Transport",
				value: draft.transport ?? "auto",
				action: () => {
					const values = ["auto", "sse", "websocket", "websocket-cached"] as const;
					const index = values.indexOf((draft.transport ?? "auto") as (typeof values)[number]);
					draft.transport = values[(index + 1) % values.length];
				},
			},
			{
				label: "Hide thinking",
				value: boolValue(draft.hideThinkingBlock),
				action: () => {
					draft.hideThinkingBlock = cycleBoolean(draft.hideThinkingBlock);
				},
			},
			{
				label: "Compact reserve tokens",
				value: String(draft.compaction?.reserveTokens ?? 16384),
				action: () =>
					this.editNonNegativeInteger(
						"Compact reserve tokens (empty clears)",
						draft.compaction?.reserveTokens,
						(value) => {
							draft.compaction = { ...draft.compaction, reserveTokens: value };
						},
					),
			},
			{
				label: "Compact recent tokens",
				value: String(draft.compaction?.keepRecentTokens ?? 20000),
				action: () =>
					this.editNonNegativeInteger(
						"Compact recent tokens (empty clears)",
						draft.compaction?.keepRecentTokens,
						(value) => {
							draft.compaction = { ...draft.compaction, keepRecentTokens: value };
						},
					),
			},
			{
				label: "Cache miss notices",
				value: boolValue(draft.showCacheMissNotices),
				action: () => {
					draft.showCacheMissNotices = cycleBoolean(draft.showCacheMissNotices);
				},
			},
			{
				label: "Retries",
				value: boolValue(draft.retry?.enabled),
				action: () => {
					draft.retry = { ...draft.retry, enabled: cycleBoolean(draft.retry?.enabled) };
				},
			},
			{
				label: "Retry attempts",
				value: String(draft.retry?.maxRetries ?? 3),
				action: () =>
					this.editNonNegativeInteger("Retry attempts (empty clears)", draft.retry?.maxRetries, (value) => {
						draft.retry = { ...draft.retry, maxRetries: value };
					}),
			},
			{
				label: "Retry base delay",
				value: `${draft.retry?.baseDelayMs ?? 2000} ms`,
				action: () =>
					this.editNonNegativeInteger("Retry base delay ms (empty clears)", draft.retry?.baseDelayMs, (value) => {
						draft.retry = { ...draft.retry, baseDelayMs: value };
					}),
			},
			{
				label: "Provider timeout",
				value:
					draft.retry?.provider?.timeoutMs === undefined ? "SDK default" : `${draft.retry.provider.timeoutMs} ms`,
				action: () =>
					this.editNonNegativeInteger(
						"Provider timeout ms (empty clears)",
						draft.retry?.provider?.timeoutMs,
						(value) => {
							draft.retry = { ...draft.retry, provider: { ...draft.retry?.provider, timeoutMs: value } };
						},
					),
			},
			{
				label: "Provider retry attempts",
				value: String(draft.retry?.provider?.maxRetries ?? "SDK default"),
				action: () =>
					this.editNonNegativeInteger(
						"Provider retry attempts (empty clears)",
						draft.retry?.provider?.maxRetries,
						(value) => {
							draft.retry = { ...draft.retry, provider: { ...draft.retry?.provider, maxRetries: value } };
						},
					),
			},
			{
				label: "Provider max retry delay",
				value: `${draft.retry?.provider?.maxRetryDelayMs ?? 60000} ms`,
				action: () =>
					this.editNonNegativeInteger(
						"Provider max retry delay ms (empty clears)",
						draft.retry?.provider?.maxRetryDelayMs,
						(value) => {
							draft.retry = { ...draft.retry, provider: { ...draft.retry?.provider, maxRetryDelayMs: value } };
						},
					),
			},
			{
				label: "Branch summary reserve",
				value: String(draft.branchSummary?.reserveTokens ?? 16384),
				action: () =>
					this.editNonNegativeInteger(
						"Branch summary reserve tokens (empty clears)",
						draft.branchSummary?.reserveTokens,
						(value) => {
							draft.branchSummary = { ...draft.branchSummary, reserveTokens: value };
						},
					),
			},
			{
				label: "Skip branch summary prompt",
				value: boolValue(draft.branchSummary?.skipPrompt),
				action: () => {
					draft.branchSummary = {
						...draft.branchSummary,
						skipPrompt: cycleBoolean(draft.branchSummary?.skipPrompt),
					};
				},
			},
			...(["minimal", "low", "medium", "high"] as const).map((level) => ({
				label: `Thinking budget · ${level}`,
				value:
					draft.thinkingBudgets?.[level] === undefined ? "Model default" : String(draft.thinkingBudgets[level]),
				action: () =>
					this.editNonNegativeInteger(
						`Thinking budget for ${level} (empty clears)`,
						draft.thinkingBudgets?.[level],
						(value) => {
							draft.thinkingBudgets = { ...draft.thinkingBudgets, [level]: value };
						},
					),
			})),
		];
	}

	private appearanceRows(): SettingsRow[] {
		const draft = this.activeDraft();
		return [
			{
				label: "Theme",
				value: draft.theme ?? "System default",
				action: () => {
					const values = [undefined, "dark", "light", "dark/light"] as const;
					const index = values.indexOf(draft.theme as (typeof values)[number]);
					draft.theme = values[(index + 1) % values.length];
					this.status = "Theme updated in draft; preview applies after Save";
				},
			},
			{
				label: "Show images",
				value: boolValue(draft.terminal?.showImages),
				action: () => {
					draft.terminal = { ...draft.terminal, showImages: cycleBoolean(draft.terminal?.showImages) };
				},
			},
			{
				label: "Auto-resize images",
				value: boolValue(draft.images?.autoResize),
				action: () => {
					draft.images = { ...draft.images, autoResize: cycleBoolean(draft.images?.autoResize) };
				},
			},
			{
				label: "Editor padding",
				value: String(draft.editorPaddingX ?? 0),
				action: () => {
					draft.editorPaddingX = ((draft.editorPaddingX ?? 0) + 1) % 4;
				},
			},
			{
				label: "Hardware cursor",
				value: boolValue(draft.showHardwareCursor),
				action: () => {
					draft.showHardwareCursor = cycleBoolean(draft.showHardwareCursor);
				},
			},
			{
				label: "Clear on shrink",
				value: boolValue(draft.terminal?.clearOnShrink),
				action: () => {
					draft.terminal = { ...draft.terminal, clearOnShrink: cycleBoolean(draft.terminal?.clearOnShrink) };
				},
			},
			{
				label: "Terminal progress",
				value: boolValue(draft.terminal?.showTerminalProgress),
				action: () => {
					draft.terminal = {
						...draft.terminal,
						showTerminalProgress: cycleBoolean(draft.terminal?.showTerminalProgress),
					};
				},
			},
			{
				label: "Block images",
				value: boolValue(draft.images?.blockImages),
				action: () => {
					draft.images = { ...draft.images, blockImages: cycleBoolean(draft.images?.blockImages) };
				},
			},
			{
				label: "Output padding",
				value: String(draft.outputPad ?? 1),
				action: () => {
					draft.outputPad = draft.outputPad === 0 ? 1 : 0;
				},
			},
			{
				label: "Image width cells",
				value: String(draft.terminal?.imageWidthCells ?? 60),
				action: () =>
					this.editNonNegativeInteger(
						"Image width in terminal cells (empty clears)",
						draft.terminal?.imageWidthCells,
						(value) => {
							draft.terminal = { ...draft.terminal, imageWidthCells: value };
						},
					),
			},
			{
				label: "Autocomplete visible",
				value: String(draft.autocompleteMaxVisible ?? 5),
				action: () =>
					this.editNonNegativeInteger(
						"Autocomplete visible rows (empty clears)",
						draft.autocompleteMaxVisible,
						(value) => {
							draft.autocompleteMaxVisible = value;
						},
					),
			},
			{
				label: "Markdown code indent",
				value: JSON.stringify(draft.markdown?.codeBlockIndent ?? "  "),
				action: () =>
					this.editSetting(
						"Markdown code block indent (empty clears)",
						draft.markdown?.codeBlockIndent ?? "",
						(value) => {
							draft.markdown = { ...draft.markdown, codeBlockIndent: value || undefined };
							return undefined;
						},
						true,
					),
			},
			{
				label: "Anthropic extra-usage warning",
				value: boolValue(draft.warnings?.anthropicExtraUsage ?? true),
				action: () => {
					draft.warnings = {
						...draft.warnings,
						anthropicExtraUsage: !draft.warnings?.anthropicExtraUsage,
					};
				},
			},
		];
	}

	private toolsRows(): SettingsRow[] {
		const draft = this.activeDraft();
		return [
			{
				label: "Skill commands",
				value: boolValue(draft.enableSkillCommands),
				action: () => {
					draft.enableSkillCommands = cycleBoolean(draft.enableSkillCommands);
				},
			},
			{
				label: "Double escape",
				value: draft.doubleEscapeAction ?? "tree",
				action: () => {
					const values = ["tree", "fork", "none"] as const;
					const index = values.indexOf((draft.doubleEscapeAction ?? "tree") as (typeof values)[number]);
					draft.doubleEscapeAction = values[(index + 1) % values.length];
				},
			},
			{
				label: "Tree filter",
				value: draft.treeFilterMode ?? "default",
				action: () => {
					const values = ["default", "no-tools", "user-only", "labeled-only", "all"] as const;
					const index = values.indexOf((draft.treeFilterMode ?? "default") as (typeof values)[number]);
					draft.treeFilterMode = values[(index + 1) % values.length];
				},
			},
			{
				label: "External editor",
				value: draft.externalEditor ?? "System default",
				action: () =>
					this.editSetting("External editor command (empty clears)", draft.externalEditor ?? "", (value) => {
						draft.externalEditor = value || undefined;
						return undefined;
					}),
			},
			{
				label: "Shell path",
				value: draft.shellPath ?? "System default",
				action: () =>
					this.editSetting("Shell path (empty clears)", draft.shellPath ?? "", (value) => {
						draft.shellPath = value || undefined;
						return undefined;
					}),
			},
			{
				label: "Shell command prefix",
				value: draft.shellCommandPrefix ?? "Not set",
				action: () =>
					this.editSetting("Shell command prefix (empty clears)", draft.shellCommandPrefix ?? "", (value) => {
						draft.shellCommandPrefix = value || undefined;
						return undefined;
					}),
			},
			{
				label: "HTTP proxy",
				value: draft.httpProxy ?? "Not set",
				action: () =>
					this.editSetting("HTTP proxy URL (empty clears)", draft.httpProxy ?? "", (value) => {
						draft.httpProxy = value || undefined;
						return undefined;
					}),
			},
			{
				label: "HTTP idle timeout",
				value: draft.httpIdleTimeoutMs === undefined ? "Default" : `${draft.httpIdleTimeoutMs} ms`,
				action: () =>
					this.editNonNegativeInteger(
						"HTTP idle timeout ms (empty clears; 0 disables)",
						draft.httpIdleTimeoutMs,
						(value) => {
							draft.httpIdleTimeoutMs = value;
						},
					),
			},
			{
				label: "WebSocket connect timeout",
				value: draft.websocketConnectTimeoutMs === undefined ? "Default" : `${draft.websocketConnectTimeoutMs} ms`,
				action: () =>
					this.editNonNegativeInteger(
						"WebSocket connect timeout ms (empty clears; 0 disables)",
						draft.websocketConnectTimeoutMs,
						(value) => {
							draft.websocketConnectTimeoutMs = value;
						},
					),
			},
			{
				label: "Enabled model patterns",
				value: draft.enabledModels?.join(", ") || "All available models",
				action: this.globalOnly(() =>
					this.editCommaSeparatedStrings(
						"Enabled model patterns (comma separated; empty clears)",
						draft.enabledModels,
						(value) => {
							draft.enabledModels = value;
						},
					),
				),
			},
			{
				label: "NPM command argv",
				value: draft.npmCommand?.join(" · ") || "npm",
				action: this.globalOnly(() =>
					this.editCommaSeparatedStrings(
						"NPM command argv (comma separated; empty clears)",
						draft.npmCommand,
						(value) => {
							draft.npmCommand = value;
						},
					),
				),
			},
			{
				label: "Session directory",
				value: draft.sessionDir ?? "Default",
				action: () =>
					this.editSetting("Session directory (empty clears)", draft.sessionDir ?? "", (value) => {
						draft.sessionDir = value || undefined;
						return undefined;
					}),
			},
		];
	}

	private securityRows(): SettingsRow[] {
		const draft = this.globalDraft;
		return [
			{
				label: "Install telemetry",
				value: boolValue(draft.enableInstallTelemetry ?? true),
				action: this.globalOnly(() => {
					draft.enableInstallTelemetry = !draft.enableInstallTelemetry;
				}),
			},
			{
				label: "Analytics",
				value: boolValue(draft.enableAnalytics),
				action: this.globalOnly(() => {
					draft.enableAnalytics = cycleBoolean(draft.enableAnalytics);
				}),
			},
			{
				label: "Tracking ID",
				value: draft.trackingId ? "Generated" : "Not generated",
				action: this.globalOnly(() => this.statusMessage("Generated when analytics is enabled")),
			},
			{
				label: "Quiet startup",
				value: boolValue(draft.quietStartup),
				action: this.globalOnly(() => {
					draft.quietStartup = cycleBoolean(draft.quietStartup);
				}),
			},
			{
				label: "Collapse changelog",
				value: boolValue(draft.collapseChangelog),
				action: this.globalOnly(() => {
					draft.collapseChangelog = cycleBoolean(draft.collapseChangelog);
				}),
			},
			{
				label: "Default project trust",
				value: draft.defaultProjectTrust ?? "ask",
				action: this.globalOnly(() => {
					const values = ["ask", "always", "never"] as const;
					const index = values.indexOf(draft.defaultProjectTrust ?? "ask");
					draft.defaultProjectTrust = values[(index + 1) % values.length];
				}),
			},
		];
	}

	private globalOnly(action: () => void): () => void {
		return () => {
			if (this.scope !== "global") {
				this.status = "This setting is available only in Global scope";
				return;
			}
			action();
		};
	}

	private rowsForPage(page: PageId): SettingsRow[] {
		if (page === "defaults") return this.defaultRows();
		if (page === "delivery") return this.deliveryRows();
		if (page === "appearance") return this.appearanceRows();
		if (page === "tools") return this.toolsRows();
		if (page === "security") return this.securityRows();
		return [];
	}

	private setCompaction(enabled: boolean): void {
		const draft = this.activeDraft();
		draft.compaction = { ...draft.compaction, enabled };
	}

	private editSetting(
		label: string,
		value: string,
		apply: (next: string) => string | undefined,
		preserveWhitespace = false,
	): void {
		this.settingEditor = { label, apply, preserveWhitespace };
		this.editorStep = "settingValue";
		this.input = new Input();
		this.input.focused = this.focused;
		this.input.setValue(value);
		this.input.onEscape = () => this.cancelEditor();
		this.input.onSubmit = (next) => this.advanceEditor(next);
	}

	private editNonNegativeInteger(
		label: string,
		current: number | undefined,
		apply: (value: number | undefined) => void,
	): void {
		this.editSetting(label, current === undefined ? "" : String(current), (next) => {
			if (!next) {
				apply(undefined);
				return undefined;
			}
			const value = Number(next);
			if (!Number.isSafeInteger(value) || value < 0) return "Enter a non-negative integer";
			apply(value);
			return undefined;
		});
	}

	private editCommaSeparatedStrings(
		label: string,
		current: readonly string[] | undefined,
		apply: (value: string[] | undefined) => void,
	): void {
		this.editSetting(label, current?.join(", ") ?? "", (next) => {
			const values = next
				.split(",")
				.map((value) => value.trim())
				.filter((value) => value.length > 0);
			apply(values.length > 0 ? values : undefined);
			return undefined;
		});
	}

	private statusMessage(message: string): void {
		this.status = message;
	}

	private openActionMenu(title: string, subtitle: string | undefined, items: SettingsActionMenu["items"]): void {
		this.actionMenu = { title, subtitle, items, index: 0 };
		this.status = "Choose an action";
	}

	private renderActionMenu(width: number): string[] {
		const menu = this.actionMenu!;
		const lines = [theme.bold(theme.fg("text", menu.title))];
		if (menu.subtitle) lines.push(theme.fg("muted", menu.subtitle));
		lines.push("");
		for (let index = 0; index < menu.items.length; index++) {
			const item = menu.items[index]!;
			const selected = index === menu.index;
			lines.push(
				truncateToWidth(
					`${selected ? theme.fg("accent", "› ") : "  "}${selected ? theme.fg("accent", item.label) : item.label}`,
					width,
					"",
				),
			);
		}
		lines.push("", theme.fg("muted", "↑↓ choose · Enter open · Esc back"));
		return lines;
	}

	private handleActionMenuInput(data: string, keybindings: ReturnType<typeof getKeybindings>): void {
		const menu = this.actionMenu!;
		if (
			keybindings.matches(data, "app.focus.left") ||
			keybindings.matches(data, "app.focus.right") ||
			keybindings.matches(data, "app.focus.up") ||
			keybindings.matches(data, "app.focus.down")
		) {
			this.actionMenu = undefined;
			this.focus = "content";
			this.status = "Action menu closed; focus returned to fields";
			return;
		}
		if (keybindings.matches(data, "tui.select.cancel")) {
			this.actionMenu = undefined;
			this.status = "Draft changes are not saved";
			return;
		}
		if (keybindings.matches(data, "tui.select.up")) {
			menu.index = Math.max(0, menu.index - 1);
			return;
		}
		if (keybindings.matches(data, "tui.select.down")) {
			menu.index = Math.min(menu.items.length - 1, menu.index + 1);
			return;
		}
		if (keybindings.matches(data, "tui.select.confirm")) {
			const item = menu.items[menu.index];
			this.actionMenu = undefined;
			item?.action();
		}
	}

	private startProviderEditor(): void {
		this.openProviderForm();
	}

	private startModelEditor(): void {
		if (this.scope !== "global") {
			this.status = "Model definitions are global";
			return;
		}
		this.modelEditor = { providerId: "", id: "", name: "", api: "", contextWindow: "", maxTokens: "" };
		this.openEditor("modelProvider");
	}

	private startModelEditorEdit(): void {
		if (this.scope !== "global") {
			this.status = "Provider and model definitions are global";
			return;
		}
		this.editTarget = undefined;
		this.openEditor("editProvider");
	}

	private startHeaderEditor(): void {
		if (this.scope !== "global") {
			this.status = "Provider and model definitions are global";
			return;
		}
		this.headerEditor = { providerId: "", name: "" };
		this.openEditor("headerProvider");
	}

	private startModelAdvancedEditor(): void {
		if (this.scope !== "global") {
			this.status = "Model definitions are global";
			return;
		}
		this.advancedTarget = undefined;
		this.costEditor = undefined;
		this.tierEditor = undefined;
		this.compatTarget = undefined;
		this.overrideTarget = undefined;
		this.openEditor("advancedProvider");
	}

	private startCompatEditor(): void {
		if (this.scope !== "global") {
			this.status = "Provider and model compatibility settings are global";
			return;
		}
		this.compatTarget = undefined;
		this.openEditor("compatProvider");
	}

	private startOverrideEditor(): void {
		if (this.scope !== "global") {
			this.status = "Model overrides are global";
			return;
		}
		this.overrideTarget = undefined;
		this.openEditor("overrideProvider");
	}

	private startModelDeletionPicker(): void {
		if (this.scope !== "global") {
			this.status = "Provider and model definitions are global";
			return;
		}
		const targets = this.modelDeletionTargets();
		if (targets.length === 0) {
			this.status = "There are no provider, model, or override definitions to delete";
			return;
		}
		this.modelDeletionPicker = { targets, index: 0, confirming: false };
		this.status = "Choose the provider, model, or override to delete";
	}

	private modelDeletionTargets(): ModelDeletionTarget[] {
		const targets: ModelDeletionTarget[] = [];
		for (const providerId of Object.keys(this.modelsDraft.providers).sort((left, right) =>
			left.localeCompare(right),
		)) {
			const provider = this.modelsDraft.providers[providerId]!;
			targets.push({ kind: "provider", providerId });
			for (const model of [...(provider.models ?? [])].sort((left, right) => left.id.localeCompare(right.id))) {
				targets.push({ kind: "model", providerId, modelId: model.id });
			}
			for (const overrideId of Object.keys(provider.modelOverrides ?? {}).sort((left, right) =>
				left.localeCompare(right),
			)) {
				targets.push({ kind: "override", providerId, overrideId });
			}
		}
		return targets;
	}

	private renderModelDeletionPicker(width: number): string[] {
		const picker = this.modelDeletionPicker!;
		const lines = [
			theme.bold(theme.fg("text", "Delete model configuration")),
			theme.fg("warning", "Deletion is staged until Ctrl+S; it does not change files yet."),
			"",
		];
		for (let index = 0; index < picker.targets.length; index++) {
			const target = picker.targets[index]!;
			const selected = index === picker.index;
			const prefix = selected ? theme.fg("accent", "› ") : "  ";
			const detail =
				target.kind === "provider"
					? `Provider · ${target.providerId}`
					: target.kind === "model"
						? `  Model · ${target.providerId} / ${target.modelId}`
						: `  Override · ${target.providerId} / ${target.overrideId}`;
			const text = selected ? theme.bold(theme.fg("accent", detail)) : detail;
			lines.push(truncateToWidth(`${prefix}${text}`, width, ""));
		}
		lines.push(
			"",
			theme.fg(
				picker.confirming ? "warning" : "muted",
				picker.confirming ? "Enter confirm deletion · Esc cancel" : "↑↓ choose · Enter select · Esc cancel",
			),
		);
		return lines;
	}

	private handleModelDeletionPickerInput(data: string, keybindings: ReturnType<typeof getKeybindings>): void {
		const picker = this.modelDeletionPicker!;
		if (keybindings.matches(data, "tui.select.cancel")) {
			this.modelDeletionPicker = undefined;
			this.status = "Model configuration deletion cancelled";
			return;
		}
		if (keybindings.matches(data, "tui.select.up") || keybindings.matches(data, "tui.select.down")) {
			const delta = keybindings.matches(data, "tui.select.up") ? -1 : 1;
			picker.index = Math.max(0, Math.min(picker.targets.length - 1, picker.index + delta));
			picker.confirming = false;
			return;
		}
		if (!keybindings.matches(data, "tui.select.confirm")) return;
		const target = picker.targets[picker.index];
		if (!target) return;
		if (!picker.confirming) {
			picker.confirming = true;
			this.status = "Confirm deletion of the selected model configuration";
			return;
		}
		const provider = this.modelsDraft.providers[target.providerId];
		if (!provider) {
			this.modelDeletionPicker = undefined;
			this.status = "The selected provider no longer exists in this draft";
			return;
		}
		if (target.kind === "provider") delete this.modelsDraft.providers[target.providerId];
		else if (target.kind === "model")
			provider.models = provider.models?.filter((model) => model.id !== target.modelId);
		else delete provider.modelOverrides?.[target.overrideId];
		if (target.kind === "provider") {
			this.providerDetail = undefined;
			this.modelDetail = undefined;
		} else if (target.kind === "model") {
			this.modelDetail = undefined;
		}
		this.modelDeletionPicker = undefined;
		this.status = "Model configuration deletion added to draft";
	}

	private openEditor(step: EditorStep): void {
		this.editorModelsBefore = structuredClone(this.modelsDraft);
		this.editorStep = step;
		this.input = new Input();
		this.input.focused = this.focused;
		this.input.onEscape = () => this.cancelEditor();
		this.input.onSubmit = (value) => this.advanceEditor(value);
	}

	private advanceEditor(value: string): void {
		if (!this.editorStep) return;
		const trimmed = value.trim();
		if (this.editorStep === "settingValue" && this.settingEditor) {
			const error = this.settingEditor.apply(this.settingEditor.preserveWhitespace ? value : trimmed);
			if (error) {
				this.status = error;
				return;
			}
			this.status = "Setting updated in draft";
			this.finishEditor();
			return;
		}
		const optional = new Set<EditorStep>([
			"providerBaseUrl",
			"providerApi",
			"modelName",
			"modelApi",
			"modelContext",
			"modelMaxTokens",
			"editModelId",
			"editProviderName",
			"editProviderBaseUrl",
			"editProviderApi",
			"editProviderAuthHeader",
			"editProviderOauth",
			"editModelName",
			"editModelApi",
			"editModelBaseUrl",
			"editModelReasoning",
			"editModelInput",
			"editModelContext",
			"editModelMaxTokens",
			"headerModelId",
			"headerValue",
			...THINKING_EDITOR_STEPS,
			"advancedTierAction",
			"compatModelId",
			"compatValue",
			"overrideName",
			"overrideReasoning",
			"overrideInput",
			"overrideContext",
			"overrideMaxTokens",
		]);
		if (!trimmed && !optional.has(this.editorStep)) {
			this.status = "This field is required";
			return;
		}
		if (this.isAdvancedEditorStep(this.editorStep)) {
			this.advanceAdvancedEditor(this.editorStep, trimmed);
			return;
		}
		if (this.isCompatEditorStep(this.editorStep)) {
			this.advanceCompatEditor(this.editorStep, trimmed);
			return;
		}
		if (this.isOverrideEditorStep(this.editorStep)) {
			this.advanceOverrideEditor(this.editorStep, trimmed);
			return;
		}
		if (this.credentialProviderId && this.editorStep === "providerCredentialApiKey") {
			if (!trimmed) {
				this.status = "API key is required; use Clear authentication to remove it";
				return;
			}
			this.credentialMutations.set(this.credentialProviderId, { type: "api_key", key: trimmed });
			this.status = "Provider API key added to draft";
			this.finishEditor();
			return;
		}
		if (this.providerEditor && this.editorStep === "providerId") {
			if (this.modelsDraft.providers[trimmed]) {
				this.status = "A provider with this ID already exists";
				return;
			}
			this.providerEditor.id = trimmed;
			this.editorStep = "providerName";
		} else if (this.providerEditor && this.editorStep === "providerName") {
			this.providerEditor.name = trimmed;
			this.editorStep = "providerBaseUrl";
		} else if (this.providerEditor && this.editorStep === "providerBaseUrl") {
			this.providerEditor.baseUrl = trimmed;
			this.editorStep = "providerApi";
		} else if (this.providerEditor && this.editorStep === "providerApi") {
			this.providerEditor.api = trimmed;
			this.modelsDraft.providers[this.providerEditor.id] = {
				...(this.providerEditor.name ? { name: this.providerEditor.name } : {}),
				...(this.providerEditor.baseUrl ? { baseUrl: this.providerEditor.baseUrl } : {}),
				...(this.providerEditor.api ? { api: this.providerEditor.api } : {}),
				models: [],
			};
			this.status = "New provider added to draft";
			this.finishEditor();
			return;
		} else if (this.modelEditor && this.editorStep === "modelProvider") {
			if (!this.modelsDraft.providers[trimmed]) {
				this.status = "Create the provider first";
				return;
			}
			this.modelEditor.providerId = trimmed;
			this.editorStep = "modelId";
		} else if (this.modelEditor && this.editorStep === "modelId") {
			this.modelEditor.id = trimmed;
			this.editorStep = "modelName";
		} else if (this.modelEditor && this.editorStep === "modelName") {
			this.modelEditor.name = trimmed;
			this.editorStep = "modelApi";
		} else if (this.modelEditor && this.editorStep === "modelApi") {
			this.modelEditor.api = trimmed;
			this.editorStep = "modelContext";
		} else if (this.modelEditor && this.editorStep === "modelContext") {
			if (trimmed && (!Number.isSafeInteger(Number(trimmed)) || Number(trimmed) <= 0)) {
				this.status = "Context window must be a positive integer";
				return;
			}
			this.modelEditor.contextWindow = trimmed;
			this.editorStep = "modelMaxTokens";
		} else if (this.modelEditor && this.editorStep === "modelMaxTokens") {
			if (trimmed && (!Number.isSafeInteger(Number(trimmed)) || Number(trimmed) <= 0)) {
				this.status = "Maximum output tokens must be a positive integer";
				return;
			}
			this.modelEditor.maxTokens = trimmed;
			const provider = this.modelsDraft.providers[this.modelEditor.providerId]!;
			const models = provider.models ?? [];
			if (models.some((model) => model.id === this.modelEditor!.id)) {
				this.status = "A model with this ID already exists for the provider";
				return;
			}
			models.push({
				id: this.modelEditor.id,
				...(this.modelEditor.name ? { name: this.modelEditor.name } : {}),
				...(this.modelEditor.api ? { api: this.modelEditor.api } : {}),
				...(this.modelEditor.contextWindow ? { contextWindow: Number(this.modelEditor.contextWindow) } : {}),
				...(this.modelEditor.maxTokens ? { maxTokens: Number(this.modelEditor.maxTokens) } : {}),
			});
			provider.models = models;
			this.status = "New model added to draft";
			this.finishEditor();
			return;
		} else if (this.editorStep === "editProvider") {
			if (!this.modelsDraft.providers[trimmed]) {
				this.status = "Provider was not found in models.json";
				return;
			}
			this.editTarget = { providerId: trimmed };
			this.editorStep = "editModelId";
		} else if (this.editTarget && this.editorStep === "editModelId") {
			if (trimmed) {
				const model = this.modelsDraft.providers[this.editTarget.providerId]?.models?.find(
					(candidate) => candidate.id === trimmed,
				);
				if (!model) {
					this.status = "Model was not found for this provider";
					return;
				}
				this.editTarget.modelId = trimmed;
				this.editorStep = "editModelName";
			} else {
				this.editorStep = "editProviderName";
			}
		} else if (this.editTarget && this.editorStep === "editProviderName") {
			this.setProviderTextField(this.editTarget.providerId, "name", trimmed);
			this.editorStep = "editProviderBaseUrl";
		} else if (this.editTarget && this.editorStep === "editProviderBaseUrl") {
			this.setProviderTextField(this.editTarget.providerId, "baseUrl", trimmed);
			this.editorStep = "editProviderApi";
		} else if (this.editTarget && this.editorStep === "editProviderApi") {
			this.setProviderTextField(this.editTarget.providerId, "api", trimmed);
			this.editorStep = "editProviderAuthHeader";
		} else if (this.editTarget && this.editorStep === "editProviderAuthHeader") {
			if (!this.setProviderBooleanField(this.editTarget.providerId, "authHeader", trimmed)) return;
			this.editorStep = "editProviderOauth";
		} else if (this.editTarget && this.editorStep === "editProviderOauth") {
			if (!this.setProviderOauth(this.editTarget.providerId, trimmed)) return;
			this.status = "Provider updated in draft";
			this.finishEditor();
			return;
		} else if (this.editTarget?.modelId && this.editorStep === "editModelName") {
			this.setModelTextField(this.editTarget.providerId, this.editTarget.modelId, "name", trimmed);
			this.editorStep = "editModelApi";
		} else if (this.editTarget?.modelId && this.editorStep === "editModelApi") {
			this.setModelTextField(this.editTarget.providerId, this.editTarget.modelId, "api", trimmed);
			this.editorStep = "editModelBaseUrl";
		} else if (this.editTarget?.modelId && this.editorStep === "editModelBaseUrl") {
			this.setModelTextField(this.editTarget.providerId, this.editTarget.modelId, "baseUrl", trimmed);
			this.editorStep = "editModelReasoning";
		} else if (this.editTarget?.modelId && this.editorStep === "editModelReasoning") {
			if (!this.setModelBooleanField(this.editTarget.providerId, this.editTarget.modelId, "reasoning", trimmed))
				return;
			this.editorStep = "editModelInput";
		} else if (this.editTarget?.modelId && this.editorStep === "editModelInput") {
			if (!this.setModelInputField(this.editTarget.providerId, this.editTarget.modelId, trimmed)) return;
			this.editorStep = "editModelContext";
		} else if (this.editTarget?.modelId && this.editorStep === "editModelContext") {
			if (trimmed && (!Number.isSafeInteger(Number(trimmed)) || Number(trimmed) <= 0)) {
				this.status = "Context window must be a positive integer";
				return;
			}
			this.setModelNumberField(this.editTarget.providerId, this.editTarget.modelId, "contextWindow", trimmed);
			this.editorStep = "editModelMaxTokens";
		} else if (this.editTarget?.modelId && this.editorStep === "editModelMaxTokens") {
			if (trimmed && (!Number.isSafeInteger(Number(trimmed)) || Number(trimmed) <= 0)) {
				this.status = "Maximum output tokens must be a positive integer";
				return;
			}
			this.setModelNumberField(this.editTarget.providerId, this.editTarget.modelId, "maxTokens", trimmed);
			this.status = "Model updated in draft";
			this.finishEditor();
			return;
		} else if (this.headerEditor && this.editorStep === "headerProvider") {
			if (!this.modelsDraft.providers[trimmed]) {
				this.status = "Provider was not found in models.json";
				return;
			}
			this.headerEditor.providerId = trimmed;
			this.editorStep = "headerModelId";
		} else if (this.headerEditor && this.editorStep === "headerModelId") {
			if (trimmed.startsWith("override:")) {
				const overrideId = trimmed.slice("override:".length);
				if (!overrideId) {
					this.status = "Override patterns use override:<model-id-or-pattern>";
					return;
				}
				const provider = this.modelsDraft.providers[this.headerEditor.providerId]!;
				provider.modelOverrides ??= {};
				provider.modelOverrides[overrideId] ??= {};
				this.headerEditor.overrideId = overrideId;
				this.editorStep = "headerName";
			} else if (trimmed) {
				const model = this.modelsDraft.providers[this.headerEditor.providerId]?.models?.find(
					(candidate) => candidate.id === trimmed,
				);
				if (!model) {
					this.status = "Model was not found for this provider";
					return;
				}
				this.headerEditor.modelId = trimmed;
			}
			this.editorStep = "headerName";
		} else if (this.headerEditor && this.editorStep === "headerName") {
			if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/u.test(trimmed)) {
				this.status = "Header name contains invalid characters";
				return;
			}
			this.headerEditor.name = trimmed;
			this.editorStep = "headerValue";
		} else if (this.headerEditor && this.editorStep === "headerValue") {
			this.setDraftHeader(
				this.headerEditor.providerId,
				this.headerEditor.modelId,
				this.headerEditor.name,
				trimmed,
				this.headerEditor.overrideId,
			);
			this.status = trimmed ? "Header updated in draft" : "Header removed from draft";
			this.finishEditor();
			return;
		}
		this.input?.setValue("");
	}

	private isAdvancedEditorStep(step: EditorStep): boolean {
		return step.startsWith("advanced");
	}

	private isCompatEditorStep(step: EditorStep): boolean {
		return step.startsWith("compat");
	}

	private isOverrideEditorStep(step: EditorStep): boolean {
		return step.startsWith("override");
	}

	private advanceOverrideEditor(step: EditorStep, value: string): void {
		if (step === "overrideProvider") {
			if (!this.modelsDraft.providers[value]) {
				this.status = "Provider was not found in models.json";
				return;
			}
			this.overrideTarget = { providerId: value, id: "" };
			this.setEditorStep("overrideId");
			return;
		}
		if (step === "overrideId") {
			if (!this.overrideTarget) throw new Error("Missing model override target");
			this.overrideTarget.id = value;
			this.overrideConfig();
			this.setEditorStep("overrideName");
			return;
		}
		const override = this.overrideConfig();
		if (step === "overrideName") {
			if (value) override.name = value;
			else delete override.name;
			this.setEditorStep("overrideReasoning");
			return;
		}
		if (step === "overrideReasoning") {
			if (!this.setOverrideBoolean(override, "reasoning", value)) return;
			this.setEditorStep("overrideInput");
			return;
		}
		if (step === "overrideInput") {
			if (!this.setOverrideInput(override, value)) return;
			this.setEditorStep("overrideContext");
			return;
		}
		if (step === "overrideContext") {
			if (value && (!Number.isSafeInteger(Number(value)) || Number(value) <= 0)) {
				this.status = "Context window must be a positive integer";
				return;
			}
			if (value) override.contextWindow = Number(value);
			else delete override.contextWindow;
			this.setEditorStep("overrideMaxTokens");
			return;
		}
		if (value && (!Number.isSafeInteger(Number(value)) || Number(value) <= 0)) {
			this.status = "Maximum output tokens must be a positive integer";
			return;
		}
		if (value) override.maxTokens = Number(value);
		else delete override.maxTokens;
		this.status = "Model override updated in draft";
		this.finishEditor();
	}

	private overrideConfig(): NonNullable<ModelsJson["providers"][string]["modelOverrides"]>[string] {
		if (!this.overrideTarget?.id) throw new Error("No model override selected");
		const provider = this.modelsDraft.providers[this.overrideTarget.providerId]!;
		const overrides = provider.modelOverrides ?? {};
		const override = overrides[this.overrideTarget.id] ?? {};
		overrides[this.overrideTarget.id] = override;
		provider.modelOverrides = overrides;
		return override;
	}

	private setOverrideBoolean(override: { reasoning?: boolean }, field: "reasoning", value: string): boolean {
		const normalized = value.toLowerCase();
		if (!normalized || normalized === "auto") {
			delete override[field];
			return true;
		}
		if (normalized === "on" || normalized === "true") {
			override[field] = true;
			return true;
		}
		if (normalized === "off" || normalized === "false") {
			override[field] = false;
			return true;
		}
		this.status = "Use auto, on, or off";
		return false;
	}

	private setOverrideInput(override: { input?: Array<"text" | "image"> }, value: string): boolean {
		const values = value
			.split(",")
			.map((entry) => entry.trim())
			.filter(Boolean);
		if (values.length === 0) {
			delete override.input;
			return true;
		}
		if (values.some((entry) => entry !== "text" && entry !== "image") || new Set(values).size !== values.length) {
			this.status = "Input modalities must be text, image, or text,image";
			return false;
		}
		override.input = values as Array<"text" | "image">;
		return true;
	}

	private advanceCompatEditor(step: EditorStep, value: string): void {
		if (step === "compatProvider") {
			if (!this.modelsDraft.providers[value]) {
				this.status = "Provider was not found in models.json";
				return;
			}
			this.compatTarget = { providerId: value };
			this.setEditorStep("compatModelId");
			return;
		}
		if (step === "compatModelId") {
			if (!this.compatTarget) throw new Error("Missing compatibility editor target");
			if (value.startsWith("override:")) {
				const overrideId = value.slice("override:".length);
				if (!overrideId) {
					this.status = "Override patterns use override:<model-id-or-pattern>";
					return;
				}
				const provider = this.modelsDraft.providers[this.compatTarget.providerId]!;
				provider.modelOverrides ??= {};
				provider.modelOverrides[overrideId] ??= {};
				this.compatTarget.overrideId = overrideId;
				this.setEditorStep("compatField");
				return;
			}
			if (value) {
				const model = this.modelsDraft.providers[this.compatTarget.providerId]?.models?.find(
					(candidate) => candidate.id === value,
				);
				if (!model) {
					this.status = "Model was not found for this provider";
					return;
				}
				this.compatTarget.modelId = value;
			}
			this.setEditorStep("compatField");
			return;
		}
		if (step === "compatField") {
			this.selectCompatField(value);
			return;
		}
		if (step === "compatTemplateName") {
			if (!/^[A-Za-z_][A-Za-z0-9_]*$/u.test(value)) {
				this.status = "Template kwarg names use letters, numbers, and underscores";
				return;
			}
			if (!this.compatTarget) throw new Error("Missing compatibility editor target");
			this.compatTarget.field = { key: `chatTemplateKwargs.${value}`, kind: "stringOrNull" };
			this.compatTarget.templateName = value;
			this.setEditorStep("compatValue", this.formatCompatValue(this.compatValue()));
			return;
		}
		this.setCompatValue(value);
	}

	private selectCompatField(value: string): void {
		if (!this.compatTarget) throw new Error("Missing compatibility editor target");
		if (value === "done") {
			this.status = "Compatibility settings updated in draft";
			this.finishEditor();
			return;
		}
		const clear = /^clear\s+(.+)$/u.exec(value);
		if (clear) {
			const field = this.resolveCompatField(clear[1]!);
			if (!field) {
				this.status = "Unknown compatibility field";
				return;
			}
			this.deleteCompatPath(this.compatRecord(), field.key);
			this.status = `${field.key} cleared from draft`;
			this.setEditorStep("compatField");
			return;
		}
		if (value === "chatTemplateKwargs") {
			this.setEditorStep("compatTemplateName");
			return;
		}
		const field = this.resolveCompatField(value);
		if (!field) {
			this.status = "Unknown field. Use a listed field, chatTemplateKwargs, clear FIELD, or done";
			return;
		}
		this.compatTarget.field = field;
		this.compatTarget.templateName = undefined;
		this.setEditorStep("compatValue", this.formatCompatValue(this.compatValue()));
	}

	private resolveCompatField(value: string): CompatField | undefined {
		return COMPAT_FIELDS.find((field) => field.key === value);
	}

	private setCompatValue(value: string): void {
		const target = this.compatTarget;
		if (!target?.field) throw new Error("Missing compatibility editor field");
		const compat = this.compatRecord();
		if (!value) {
			this.deleteCompatPath(compat, target.field.key);
			this.status = `${target.field.key} cleared from draft`;
			this.setEditorStep("compatField");
			return;
		}
		const parsed = this.parseCompatValue(target.field, value, target.templateName);
		if (parsed === undefined) return;
		this.setCompatPath(compat, target.field.key, parsed);
		this.setCompatRecord(compat);
		this.status = `${target.field.key} updated in draft`;
		this.setEditorStep("compatField");
	}

	private parseCompatValue(field: CompatField, value: string, templateName?: string): unknown | undefined {
		if (templateName) {
			if (value === "null") return null;
			if (value.startsWith("string:")) return value.slice("string:".length);
			if (value.startsWith("number:")) return this.parseNumber(value.slice("number:".length), "Template number");
			if (value.startsWith("boolean:")) {
				const booleanValue = this.parseBoolean(value.slice("boolean:".length));
				if (booleanValue === undefined) this.status = "Template booleans use boolean:on or boolean:off";
				return booleanValue;
			}
			if (value.startsWith("variable:")) {
				const [variable, option] = value.slice("variable:".length).split(":", 2);
				if (variable !== "thinking.enabled" && variable !== "thinking.effort") {
					this.status = "Template variables are thinking.enabled or thinking.effort";
					return undefined;
				}
				if (option && option !== "omitWhenOff") {
					this.status = "Variable option is omitWhenOff";
					return undefined;
				}
				return { $var: variable, ...(option ? { omitWhenOff: true } : {}) };
			}
			this.status = "Template values use string:, number:, boolean:, null, or variable:";
			return undefined;
		}
		switch (field.kind) {
			case "boolean": {
				const booleanValue = this.parseBoolean(value);
				if (booleanValue === undefined) this.status = "Use on or off";
				return booleanValue;
			}
			case "enum":
				if (!field.values?.includes(value)) {
					this.status = `Use one of: ${field.values?.join(", ")}`;
					return undefined;
				}
				return value;
			case "list":
				return value
					.split(",")
					.map((entry) => entry.trim())
					.filter(Boolean);
			case "number":
				return this.parseNumber(value, field.key);
			case "numberOrString": {
				const numberValue = Number(value);
				return Number.isFinite(numberValue) ? numberValue : value;
			}
			case "stringOrNull":
				return value === "null" ? null : value;
		}
	}

	private parseBoolean(value: string): boolean | undefined {
		const normalized = value.toLowerCase();
		if (normalized === "on" || normalized === "true") return true;
		if (normalized === "off" || normalized === "false") return false;
		return undefined;
	}

	private compatRecord(): Record<string, unknown> {
		const target = this.compatTargetConfig() as { compat?: Record<string, unknown> };
		return structuredClone(target.compat ?? {});
	}

	private setCompatRecord(compat: Record<string, unknown>): void {
		const target = this.compatTargetConfig() as { compat?: Record<string, unknown> };
		if (Object.keys(compat).length === 0) delete target.compat;
		else target.compat = compat;
	}

	private compatTargetConfig(): object {
		if (!this.compatTarget) throw new Error("No provider or model selected for compatibility editing");
		if (this.compatTarget.overrideId) {
			return this.modelsDraft.providers[this.compatTarget.providerId]!.modelOverrides![
				this.compatTarget.overrideId
			]!;
		}
		if (this.compatTarget.modelId)
			return this.findDraftModel(this.compatTarget.providerId, this.compatTarget.modelId);
		return this.modelsDraft.providers[this.compatTarget.providerId]!;
	}

	private compatValue(): unknown {
		const field = this.compatTarget?.field;
		return field ? this.getCompatPath(this.compatRecord(), field.key) : undefined;
	}

	private getCompatPath(source: Record<string, unknown>, path: string): unknown {
		let current: unknown = source;
		for (const part of path.split(".")) {
			if (typeof current !== "object" || current === null || Array.isArray(current)) return undefined;
			current = (current as Record<string, unknown>)[part];
		}
		return current;
	}

	private setCompatPath(target: Record<string, unknown>, path: string, value: unknown): void {
		const parts = path.split(".");
		let current = target;
		for (const part of parts.slice(0, -1)) {
			const existing = current[part];
			if (typeof existing !== "object" || existing === null || Array.isArray(existing)) current[part] = {};
			current = current[part] as Record<string, unknown>;
		}
		current[parts.at(-1)!] = value;
	}

	private deleteCompatPath(target: Record<string, unknown>, path: string): void {
		const parts = path.split(".");
		const remove = (current: Record<string, unknown>, index: number): boolean => {
			const key = parts[index]!;
			if (index === parts.length - 1) delete current[key];
			else {
				const child = current[key];
				if (typeof child !== "object" || child === null || Array.isArray(child))
					return Object.keys(current).length === 0;
				if (remove(child as Record<string, unknown>, index + 1)) delete current[key];
			}
			return Object.keys(current).length === 0;
		};
		remove(target, 0);
		this.setCompatRecord(target);
	}

	private formatCompatValue(value: unknown): string {
		if (value === undefined) return "";
		if (value === null) return "null";
		if (Array.isArray(value)) return value.join(",");
		if (typeof value === "object") return "variable:thinking.enabled";
		return String(value);
	}

	private renderCompatSummary(width: number): string[] {
		const target = this.compatTarget!;
		const configured = Object.keys(this.compatRecord());
		return [
			truncateToWidth(
				theme.fg(
					"muted",
					`${target.providerId}${target.overrideId ? ` / ${target.overrideId}` : target.modelId ? ` / ${target.modelId}` : " (provider)"}`,
				),
				width,
				"",
			),
			truncateToWidth(theme.fg("muted", `Configured groups: ${configured.join(", ") || "none"}`), width, ""),
			truncateToWidth(
				theme.fg(
					"dim",
					"Fields: supportsDeveloperRole · thinkingFormat · openRouterRouting.* · vercelGatewayRouting.*",
				),
				width,
				"",
			),
			"",
		];
	}

	private advanceAdvancedEditor(step: EditorStep, value: string): void {
		if (step === "advancedProvider") {
			if (!this.modelsDraft.providers[value]) {
				this.status = "Provider was not found in models.json";
				return;
			}
			this.advancedTarget = { providerId: value, modelId: "" };
			this.setEditorStep("advancedModelId");
			return;
		}
		if (step === "advancedModelId") {
			const providerId = this.editorProviderId();
			if (value.startsWith("override:")) {
				const overrideId = value.slice("override:".length);
				if (!overrideId) {
					this.status = "Override patterns use override:<model-id-or-pattern>";
					return;
				}
				const provider = this.modelsDraft.providers[providerId]!;
				provider.modelOverrides ??= {};
				provider.modelOverrides[overrideId] ??= {};
				this.advancedTarget = { providerId, modelId: overrideId, overrideId };
				this.beginThinkingEditor();
				return;
			}
			const selected = this.modelsDraft.providers[providerId]?.models?.find((candidate) => candidate.id === value);
			if (!selected) {
				this.status = "Model was not found for this provider";
				return;
			}
			this.advancedTarget = { providerId, modelId: value };
			this.beginThinkingEditor();
			return;
		}
		if (THINKING_EDITOR_STEPS.includes(step as (typeof THINKING_EDITOR_STEPS)[number])) {
			this.advanceThinkingEditor(step as (typeof THINKING_EDITOR_STEPS)[number], value);
			return;
		}
		if (step === "advancedCostInput") {
			if (value.toLowerCase() === "clear") {
				delete this.advancedModel().cost;
				this.status = "Model cost settings removed from draft";
				this.finishEditor();
				return;
			}
			const rate = this.parseNumber(value, "Input cost");
			if (rate === undefined) return;
			this.costEditor = { input: rate };
			this.setEditorStep("advancedCostOutput", String(this.advancedModel().cost?.output ?? ""));
			return;
		}
		if (step === "advancedCostOutput") {
			const rate = this.parseNumber(value, "Output cost");
			if (rate === undefined) return;
			this.costEditor = { ...this.costEditor, output: rate };
			this.setEditorStep("advancedCostCacheRead", String(this.advancedModel().cost?.cacheRead ?? ""));
			return;
		}
		if (step === "advancedCostCacheRead") {
			const rate = this.parseNumber(value, "Cache read cost");
			if (rate === undefined) return;
			this.costEditor = { ...this.costEditor, cacheRead: rate };
			this.setEditorStep("advancedCostCacheWrite", String(this.advancedModel().cost?.cacheWrite ?? ""));
			return;
		}
		if (step === "advancedCostCacheWrite") {
			const rate = this.parseNumber(value, "Cache write cost");
			if (rate === undefined) return;
			const draft = { ...this.costEditor, cacheWrite: rate };
			if (draft.input === undefined || draft.output === undefined || draft.cacheRead === undefined) {
				throw new Error("Incomplete model cost editor state");
			}
			const previousTiers = this.advancedModel().cost?.tiers;
			this.advancedModel().cost = {
				input: draft.input,
				output: draft.output,
				cacheRead: draft.cacheRead,
				cacheWrite: rate,
				...(previousTiers?.length ? { tiers: structuredClone(previousTiers) } : {}),
			};
			this.costEditor = undefined;
			this.setEditorStep("advancedTierAction");
			return;
		}
		if (step === "advancedTierAction") {
			this.advanceTierAction(value);
			return;
		}
		this.advanceTierEditor(step, value);
	}

	private editorProviderId(): string {
		if (!this.advancedTarget?.providerId) {
			throw new Error("Missing advanced model editor provider");
		}
		return this.advancedTarget.providerId;
	}

	private beginThinkingEditor(): void {
		this.setThinkingEditorStep(THINKING_EDITOR_STEPS[0]);
	}

	private advanceThinkingEditor(step: (typeof THINKING_EDITOR_STEPS)[number], value: string): void {
		const model = this.advancedModel();
		const level = THINKING_LEVEL_BY_EDITOR_STEP[step];
		const map = { ...(model.thinkingLevelMap ?? {}) } as Record<string, string | null>;
		if (!value) delete map[level];
		else map[level] = value === "-" ? null : value;
		if (Object.keys(map).length === 0) delete model.thinkingLevelMap;
		else model.thinkingLevelMap = map as typeof model.thinkingLevelMap;

		const index = THINKING_EDITOR_STEPS.indexOf(step);
		const next = THINKING_EDITOR_STEPS[index + 1];
		if (next) {
			this.setThinkingEditorStep(next);
			return;
		}
		this.setEditorStep("advancedCostInput", String(model.cost?.input ?? ""));
	}

	private setThinkingEditorStep(step: (typeof THINKING_EDITOR_STEPS)[number]): void {
		const level = THINKING_LEVEL_BY_EDITOR_STEP[step];
		const map = this.advancedModel().thinkingLevelMap as Record<string, string | null> | undefined;
		const value = map?.[level];
		this.setEditorStep(step, value === null ? "-" : (value ?? ""));
	}

	private advanceTierAction(value: string): void {
		const action = value.toLowerCase();
		const model = this.advancedModel();
		if (!action || action === "done") {
			this.status = "Model behavior and cost updated in draft";
			this.finishEditor();
			return;
		}
		if (action === "clear") {
			if (model.cost) delete model.cost.tiers;
			this.status = "Cost tiers cleared; add or done";
			this.setEditorStep("advancedTierAction");
			return;
		}
		if (action === "add") {
			this.tierEditor = {};
			this.setEditorStep("advancedTierThreshold");
			return;
		}
		const remove = /^remove\s+(\d+)$/u.exec(action);
		if (remove) {
			const index = Number(remove[1]) - 1;
			const tiers = model.cost?.tiers;
			if (!tiers || index < 0 || index >= tiers.length) {
				this.status = "Tier number was not found";
				return;
			}
			tiers.splice(index, 1);
			if (tiers.length === 0) delete model.cost?.tiers;
			this.status = `Removed cost tier ${index + 1}`;
			this.setEditorStep("advancedTierAction");
			return;
		}
		this.status = "Use add, remove N, clear, or done";
	}

	private advanceTierEditor(step: EditorStep, value: string): void {
		if (!this.tierEditor) throw new Error("Missing cost tier editor state");
		if (step === "advancedTierThreshold") {
			const threshold = this.parseInteger(value, "Tier input tokens");
			if (threshold === undefined) return;
			this.tierEditor.inputTokensAbove = threshold;
			this.setEditorStep("advancedTierInput");
			return;
		}
		const rate = this.parseNumber(value, "Tier cost");
		if (rate === undefined) return;
		if (step === "advancedTierInput") {
			this.tierEditor.input = rate;
			this.setEditorStep("advancedTierOutput");
			return;
		}
		if (step === "advancedTierOutput") {
			this.tierEditor.output = rate;
			this.setEditorStep("advancedTierCacheRead");
			return;
		}
		if (step === "advancedTierCacheRead") {
			this.tierEditor.cacheRead = rate;
			this.setEditorStep("advancedTierCacheWrite");
			return;
		}
		if (step !== "advancedTierCacheWrite") throw new Error(`Unexpected cost tier editor step: ${step}`);
		this.tierEditor.cacheWrite = rate;
		const tier = this.tierEditor;
		if (
			tier.inputTokensAbove === undefined ||
			tier.input === undefined ||
			tier.output === undefined ||
			tier.cacheRead === undefined
		) {
			throw new Error("Incomplete cost tier editor state");
		}
		const cost = this.advancedModel().cost;
		if (!cost) throw new Error("Cannot add a tier without base model costs");
		const tiers = cost.tiers ?? [];
		tiers.push({
			inputTokensAbove: tier.inputTokensAbove,
			input: tier.input,
			output: tier.output,
			cacheRead: tier.cacheRead,
			cacheWrite: rate,
		});
		tiers.sort((a, b) => a.inputTokensAbove - b.inputTokensAbove);
		cost.tiers = tiers;
		this.tierEditor = undefined;
		this.settingEditor = undefined;
		this.setEditorStep("advancedTierAction");
	}

	private advancedModel():
		| NonNullable<ModelsJson["providers"][string]["models"]>[number]
		| NonNullable<ModelsJson["providers"][string]["modelOverrides"]>[string] {
		if (!this.advancedTarget) throw new Error("No model selected for advanced editing");
		if (this.advancedTarget.overrideId) {
			return this.modelsDraft.providers[this.advancedTarget.providerId]!.modelOverrides![
				this.advancedTarget.overrideId
			]!;
		}
		return this.findDraftModel(this.advancedTarget.providerId, this.advancedTarget.modelId);
	}

	private setEditorStep(step: EditorStep, value = ""): void {
		this.editorStep = step;
		this.input?.setValue(value);
	}

	private parseNumber(value: string, label: string): number | undefined {
		const parsed = Number(value);
		if (!Number.isFinite(parsed)) {
			this.status = `${label} must be a finite number`;
			return undefined;
		}
		return parsed;
	}

	private parseInteger(value: string, label: string): number | undefined {
		const parsed = Number(value);
		if (!Number.isSafeInteger(parsed) || parsed < 0) {
			this.status = `${label} must be a non-negative integer`;
			return undefined;
		}
		return parsed;
	}

	private renderAdvancedSummary(width: number): string[] {
		const model = this.advancedModel();
		const thinking = model.thinkingLevelMap
			? `${Object.keys(model.thinkingLevelMap).length} mapped levels`
			: "provider default";
		const cost = model.cost
			? `${model.cost.input}/${model.cost.output} input/output · ${model.cost.tiers?.length ?? 0} tiers`
			: "not configured";
		return [
			truncateToWidth(
				theme.fg("muted", `${this.advancedTarget!.providerId} / ${this.advancedTarget!.modelId}`),
				width,
				"",
			),
			truncateToWidth(theme.fg("muted", `Thinking ${thinking} · Cost ${cost}`), width, ""),
			"",
		];
	}

	private cancelEditor(): void {
		if (this.editorModelsBefore) this.modelsDraft = this.editorModelsBefore;
		this.finishEditor();
	}

	private finishEditor(): void {
		this.editorStep = undefined;
		this.providerEditor = undefined;
		this.modelEditor = undefined;
		this.credentialProviderId = undefined;
		this.editTarget = undefined;
		this.headerEditor = undefined;
		this.advancedTarget = undefined;
		this.costEditor = undefined;
		this.tierEditor = undefined;
		this.input = undefined;
		this.editorModelsBefore = undefined;
	}

	private setDraftHeader(
		providerId: string,
		modelId: string | undefined,
		name: string,
		value: string,
		overrideId?: string,
	): void {
		const provider = this.modelsDraft.providers[providerId]!;
		let target: typeof provider | ReturnType<typeof this.findDraftModel>;
		if (overrideId) {
			const overrides = provider.modelOverrides!;
			let override = overrides[overrideId];
			if (!override) {
				override = {};
				overrides[overrideId] = override;
			}
			target = override;
		} else if (modelId) {
			target = this.findDraftModel(providerId, modelId);
		} else {
			target = provider;
		}
		const headers = { ...(target.headers ?? {}) };
		const existingName = Object.keys(headers).find((candidate) => candidate.toLowerCase() === name.toLowerCase());
		if (existingName) delete headers[existingName];
		if (value) headers[name] = value;
		if (Object.keys(headers).length > 0) target.headers = headers;
		else delete target.headers;
	}

	private setProviderTextField(providerId: string, field: "name" | "baseUrl" | "api", value: string): void {
		const provider = this.modelsDraft.providers[providerId]!;
		if (value) provider[field] = value;
		else delete provider[field];
	}

	private setProviderBooleanField(providerId: string, field: "authHeader", value: string): boolean {
		const normalized = value.toLowerCase();
		const provider = this.modelsDraft.providers[providerId]!;
		if (!normalized || normalized === "auto") {
			delete provider[field];
			return true;
		}
		if (normalized === "on" || normalized === "true") {
			provider[field] = true;
			return true;
		}
		if (normalized === "off" || normalized === "false") {
			provider[field] = false;
			return true;
		}
		this.status = "Use auto, on, or off";
		return false;
	}

	private setProviderOauth(providerId: string, value: string): boolean {
		const normalized = value.toLowerCase();
		const provider = this.modelsDraft.providers[providerId]!;
		if (!normalized || normalized === "off") {
			delete provider.oauth;
			return true;
		}
		if (normalized === "radius") {
			provider.oauth = "radius";
			return true;
		}
		this.status = "OAuth type must be radius or off";
		return false;
	}

	private setModelTextField(
		providerId: string,
		modelId: string,
		field: "name" | "api" | "baseUrl",
		value: string,
	): void {
		const model = this.findDraftModel(providerId, modelId);
		if (value) model[field] = value;
		else delete model[field];
	}

	private setModelBooleanField(providerId: string, modelId: string, field: "reasoning", value: string): boolean {
		const normalized = value.toLowerCase();
		const model = this.findDraftModel(providerId, modelId);
		if (!normalized || normalized === "auto") {
			delete model[field];
			return true;
		}
		if (normalized === "on" || normalized === "true") {
			model[field] = true;
			return true;
		}
		if (normalized === "off" || normalized === "false") {
			model[field] = false;
			return true;
		}
		this.status = "Use auto, on, or off";
		return false;
	}

	private setModelInputField(providerId: string, modelId: string, value: string): boolean {
		const normalized = value
			.split(",")
			.map((item) => item.trim().toLowerCase())
			.filter(Boolean);
		const model = this.findDraftModel(providerId, modelId);
		if (normalized.length === 0) {
			delete model.input;
			return true;
		}
		if (
			normalized.some((item) => item !== "text" && item !== "image") ||
			new Set(normalized).size !== normalized.length
		) {
			this.status = "Input modalities must be text, image, or text,image";
			return false;
		}
		model.input = normalized as Array<"text" | "image">;
		return true;
	}

	private setModelNumberField(
		providerId: string,
		modelId: string,
		field: "contextWindow" | "maxTokens",
		value: string,
	): void {
		const model = this.findDraftModel(providerId, modelId);
		if (value) model[field] = Number(value);
		else delete model[field];
	}

	private findDraftModel(
		providerId: string,
		modelId: string,
	): NonNullable<ModelsJson["providers"][string]["models"]>[number] {
		const model = this.modelsDraft.providers[providerId]?.models?.find((candidate) => candidate.id === modelId);
		if (!model) throw new Error("Model was not found in the settings draft");
		return model;
	}

	private async save(): Promise<void> {
		if (this.saving) return;
		if (this.scope === "project" && !this.options.projectTrusted) {
			this.status = "Project is untrusted and cannot be saved";
			return;
		}
		this.saving = true;
		this.status = "Saving and validating...";
		try {
			await this.options.resources?.settingsManager.flush();
			await this.options.onSave({
				global: this.settingsWithResourceDraft("global"),
				project: this.settingsWithResourceDraft("project"),
				models: structuredClone(this.modelsDraft),
				credentials: [...this.credentialMutations.entries()].map(([providerId, credential]) => ({
					providerId,
					credential: credential ? structuredClone(credential) : undefined,
				})),
			});
			this.status = "Saved — changes are applied. Esc closes Settings.";
		} catch (error) {
			if (error instanceof SettingsCenterSaveError) {
				this.pageIndex = PAGES.findIndex((page) => page.id === error.page);
				this.focus = "content";
				this.rowIndex = error.rowIndex ?? 0;
				this.bodyScroll = 0;
				this.manualBodyScroll = false;
				this.openProviderErrorTarget(error.providerTarget);
			}
			this.status = error instanceof Error ? error.message : String(error);
		} finally {
			this.saving = false;
		}
	}

	private openProviderErrorTarget(target: SettingsProviderErrorTarget | undefined): void {
		this.providerDetail = undefined;
		this.modelDetail = undefined;
		if (!target?.providerId || !this.modelsDraft.providers[target.providerId]) return;
		this.openProviderDetail(target.providerId);
		if (target.modelId) this.openModelDetail(target.providerId, target.modelId);
	}
}
