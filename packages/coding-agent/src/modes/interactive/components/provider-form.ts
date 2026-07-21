import { type Component, type Focusable, fuzzyFilter, getKeybindings, Input, truncateToWidth } from "@frelion/bone-tui";
import type { ModelsJsonModel, ModelsJsonModelOverride, ModelsJsonProvider } from "../../../core/model-config.ts";
import type { ProviderPreset } from "../../../core/provider-presets.ts";
import { theme } from "../theme/theme.ts";
import type { ProviderAuthenticationStatus } from "./model-settings-navigator.ts";

export type ProviderFormDraft = {
	id: string;
	provider: ModelsJsonProvider;
};

export type ProviderFormCallbacks = {
	onDraftChange?: (draft: ProviderFormDraft) => void;
	onStageApiKey: (providerId: string, apiKey: string) => void | Promise<void>;
	onStageOAuth: (providerId: string) => void | Promise<void>;
	onClearAuthentication: (providerId: string) => void | Promise<void>;
	onFetchModels?: (draft: ProviderFormDraft, stagedApiKey: string | undefined) => Promise<readonly ModelsJsonModel[]>;
};

export type ProviderFormProps = {
	mode: "create" | "edit";
	draft?: ProviderFormDraft;
	presets: readonly ProviderPreset[];
	authentication: ProviderAuthenticationStatus;
	getAuthentication?: (providerId: string) => ProviderAuthenticationStatus;
	callbacks: ProviderFormCallbacks;
};

type FormItem =
	| { kind: "template" }
	| { kind: "id" }
	| { kind: "name" }
	| { kind: "base-url" }
	| { kind: "api" }
	| { kind: "api-key" }
	| { kind: "oauth" }
	| { kind: "clear-auth" }
	| { kind: "models" }
	| { kind: "model"; modelId: string }
	| { kind: "model-name"; modelId: string }
	| { kind: "model-api"; modelId: string }
	| { kind: "model-base-url"; modelId: string }
	| { kind: "model-reasoning"; modelId: string }
	| { kind: "model-input"; modelId: string }
	| { kind: "model-context-window"; modelId: string }
	| { kind: "model-max-tokens"; modelId: string }
	| { kind: "model-advanced"; modelId: string }
	| { kind: "model-thinking"; modelId: string; level: ThinkingLevel }
	| { kind: "model-cost"; modelId: string; rate: CostRate }
	| { kind: "model-headers"; modelId: string }
	| { kind: "model-header"; modelId: string; name: string }
	| { kind: "model-add-header"; modelId: string }
	| { kind: "model-compat"; modelId: string }
	| { kind: "model-compat-boolean"; modelId: string; option: CompatBooleanOption }
	| { kind: "model-compat-enum"; modelId: string; option: CompatEnumOption }
	| { kind: "remove-model"; modelId: string }
	| { kind: "add-model" }
	| { kind: "fetch-models" }
	| { kind: "fetched-filter" }
	| { kind: "fetched-model"; modelId: string }
	| { kind: "add-fetched-models" }
	| { kind: "close-fetched-models" }
	| { kind: "advanced" }
	| { kind: "provider-auth-header" }
	| { kind: "provider-oauth-implementation" }
	| { kind: "provider-headers" }
	| { kind: "provider-header"; name: string }
	| { kind: "provider-add-header" }
	| { kind: "provider-compat" }
	| { kind: "provider-compat-boolean"; option: CompatBooleanOption }
	| { kind: "provider-compat-enum"; option: CompatEnumOption }
	| { kind: "provider-overrides" }
	| { kind: "model-override"; overrideId: string }
	| { kind: "override-name"; overrideId: string }
	| { kind: "override-reasoning"; overrideId: string }
	| { kind: "override-input"; overrideId: string }
	| { kind: "override-context-window"; overrideId: string }
	| { kind: "override-max-tokens"; overrideId: string }
	| { kind: "remove-override"; overrideId: string }
	| { kind: "add-override" };

type TextField =
	| "id"
	| "name"
	| "base-url"
	| "api-key"
	| "add-model"
	| "model-name"
	| "model-api"
	| "model-base-url"
	| "model-context-window"
	| "model-max-tokens"
	| "model-thinking"
	| "model-cost"
	| "header-name"
	| "header-value"
	| "override-id"
	| "override-name"
	| "override-context-window"
	| "override-max-tokens";

type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";
type CostRate = "input" | "output" | "cacheRead" | "cacheWrite";
type HeaderTarget = { scope: "provider" } | { scope: "model"; modelId: string };
type CompatBooleanOption = { key: string; label: string };
type CompatEnumOption = { key: string; label: string; values: readonly string[] };

const THINKING_LEVELS: readonly ThinkingLevel[] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const COST_RATES: readonly CostRate[] = ["input", "output", "cacheRead", "cacheWrite"];
const COMMON_COMPAT_BOOLEAN_OPTIONS: readonly CompatBooleanOption[] = [
	{ key: "supportsStore", label: "Store requests" },
	{ key: "supportsDeveloperRole", label: "Developer role" },
	{ key: "supportsReasoningEffort", label: "Reasoning effort" },
	{ key: "supportsUsageInStreaming", label: "Usage in streaming" },
	{ key: "requiresToolResultName", label: "Tool result name" },
	{ key: "requiresThinkingAsText", label: "Thinking as text" },
	{ key: "supportsStrictMode", label: "Strict structured output" },
	{ key: "supportsLongCacheRetention", label: "Long cache retention" },
	{ key: "supportsToolSearch", label: "Tool search" },
	{ key: "supportsEagerToolInputStreaming", label: "Eager tool input streaming" },
	{ key: "supportsCacheControlOnTools", label: "Cache control on tools" },
	{ key: "forceAdaptiveThinking", label: "Adaptive thinking" },
];
const COMMON_COMPAT_ENUM_OPTIONS: readonly CompatEnumOption[] = [
	{ key: "maxTokensField", label: "Maximum token field", values: ["max_completion_tokens", "max_tokens"] },
	{
		key: "thinkingFormat",
		label: "Thinking format",
		values: ["openai", "openrouter", "together", "deepseek", "zai", "qwen", "chat-template", "string-thinking"],
	},
	{ key: "cacheControlFormat", label: "Cache control format", values: ["anthropic"] },
	{
		key: "sessionAffinityFormat",
		label: "Session affinity format",
		values: ["openai", "openai-nosession", "openrouter"],
	},
];

type EditingState = {
	field: TextField;
	input: Input;
	modelId?: string;
	level?: ThinkingLevel;
	rate?: CostRate;
};

const CUSTOM_TEMPLATE_ID = "custom";
const CUSTOM_TEMPLATE_NAME = "Custom / OpenAI Compatible";
const API_PROTOCOLS = [
	{ id: "openai-completions", label: "OpenAI Compatible · Chat Completions" },
	{ id: "openai-responses", label: "OpenAI · Responses" },
	{ id: "anthropic-messages", label: "Anthropic · Messages" },
	{ id: "google-generative-ai", label: "Google · Generative AI" },
	{ id: "google-vertex", label: "Google · Vertex AI" },
	{ id: "azure-openai-responses", label: "Azure OpenAI · Responses" },
	{ id: "mistral-conversations", label: "Mistral · Conversations" },
	{ id: "bedrock-converse-stream", label: "AWS Bedrock · Converse" },
	{ id: "openai-codex-responses", label: "OpenAI Codex · Responses" },
	{ id: "pi-messages", label: "Pi Messages · Extension" },
] as const;

function customProvider(): ModelsJsonProvider {
	return { api: "openai-completions", models: [] };
}

function cloneDraft(draft: ProviderFormDraft): ProviderFormDraft {
	return structuredClone(draft);
}

function triState(value: boolean | undefined): string {
	return value === undefined ? "Automatic" : value ? "On" : "Off";
}

function booleanState(value: boolean | undefined): string {
	return value === undefined ? "Automatic" : value ? "On" : "Off";
}

function numberState(value: number | undefined): string {
	return value === undefined ? "Not set" : value.toString();
}

function cycleTriState(value: boolean | undefined): boolean | undefined {
	return value === undefined ? true : value ? false : undefined;
}

/**
 * Reusable in-memory Provider form. It deliberately owns the complete basic
 * configuration and the commonly-used model tuning fields, so users don't
 * have to navigate through a succession of one-field detail screens.
 * Persistence and credentials remain owned by the Settings transaction.
 */
export class ProviderFormComponent implements Component, Focusable {
	private readonly props: ProviderFormProps;
	private draft: ProviderFormDraft;
	private selectedIndex = 0;
	private templateId = CUSTOM_TEMPLATE_ID;
	private editing: EditingState | undefined;
	private templateSearch = new Input();
	private templatePickerOpen = false;
	private templatePickerIndex = 0;
	private protocolPickerOpen = false;
	private protocolPickerIndex = 0;
	private fetchedFilter = new Input();
	private fetchedModels: ModelsJsonModel[] | undefined;
	private fetchedModelPickerOpen = false;
	private selectedFetchedModelIds = new Set<string>();
	private advancedExpanded = false;
	private expandedModelId: string | undefined;
	private modelAdvancedExpanded = false;
	private providerHeadersExpanded = false;
	private modelHeadersExpanded = false;
	private providerCompatExpanded = false;
	private modelCompatExpanded = false;
	private overridesExpanded = false;
	private expandedOverrideId: string | undefined;
	private headerEdit: { target: HeaderTarget; name?: string } | undefined;
	private stagedApiKey: string | undefined;
	private authenticationAction: "api_key" | "oauth" | "cleared" | undefined;
	private status = "Draft changes are not saved";
	private fetchingModels = false;
	private _focused = false;

	constructor(props: ProviderFormProps) {
		this.props = props;
		this.draft = cloneDraft(props.draft ?? { id: "", provider: customProvider() });
		this.templateId = this.findTemplateId(this.draft) ?? CUSTOM_TEMPLATE_ID;
		this.templateSearch.onSubmit = () => this.selectTemplate();
		this.templateSearch.onEscape = () => this.closeTemplatePicker();
		this.fetchedFilter.onEscape = () => this.closeFetchedModels();
	}

	get focused(): boolean {
		return this._focused;
	}

	set focused(value: boolean) {
		this._focused = value;
		if (this.editing) this.editing.input.focused = value;
		this.templateSearch.focused = value && this.templatePickerOpen;
		this.fetchedFilter.focused = value && this.fetchedModelPickerOpen;
	}

	invalidate(): void {}

	getDraft(): ProviderFormDraft {
		return cloneDraft(this.draft);
	}

	/** Approximate the active row so the Settings viewport keeps inline pickers visible. */
	selectedLine(): number {
		return 2 + this.selectedIndex;
	}

	/** Close an inline editor or picker before a parent handles Escape. */
	closeTransientState(): boolean {
		if (this.editing) {
			this.closeEditor();
			return true;
		}
		if (this.templatePickerOpen) {
			this.closeTemplatePicker();
			return true;
		}
		if (this.protocolPickerOpen) {
			this.closeProtocolPicker();
			return true;
		}
		if (this.fetchedModelPickerOpen) {
			this.closeFetchedModels();
			return true;
		}
		return false;
	}

	render(width: number): string[] {
		const lines = [theme.bold(theme.fg("text", `${this.props.mode === "create" ? "Add" : "Edit"} provider`))];
		lines.push(theme.fg("muted", "Basic configuration"));
		for (const item of this.items()) {
			lines.push(...this.renderItem(item, width));
		}
		if (this.status) lines.push("", truncateToWidth(theme.fg("muted", this.status), width, ""));
		return lines;
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (this.editing) {
			if (keybindings.matches(data, "tui.select.cancel")) {
				this.closeTransientState();
				return;
			}
			this.editing.input.handleInput(data);
			return;
		}

		if (this.isTemplatePickerOpen()) {
			if (keybindings.matches(data, "tui.select.cancel")) {
				this.closeTransientState();
				return;
			}
			if (keybindings.matches(data, "tui.select.up")) {
				this.moveTemplatePicker(-1);
				return;
			}
			if (keybindings.matches(data, "tui.select.down")) {
				this.moveTemplatePicker(1);
				return;
			}
			if (keybindings.matches(data, "tui.select.confirm")) {
				this.selectTemplate();
				return;
			}
			this.templateSearch.handleInput(data);
			this.templatePickerIndex = 0;
			return;
		}

		if (this.protocolPickerOpen) {
			if (keybindings.matches(data, "tui.select.cancel")) {
				this.closeProtocolPicker();
				return;
			}
			if (keybindings.matches(data, "tui.select.up")) {
				this.moveProtocolPicker(-1);
				return;
			}
			if (keybindings.matches(data, "tui.select.down")) {
				this.moveProtocolPicker(1);
				return;
			}
			if (keybindings.matches(data, "tui.select.confirm")) this.selectProtocol();
			return;
		}

		if (this.isFetchedModelPickerOpen()) {
			if (keybindings.matches(data, "tui.select.cancel")) {
				this.closeTransientState();
				return;
			}
			if (keybindings.matches(data, "tui.select.up")) {
				this.move(-1);
				return;
			}
			if (keybindings.matches(data, "tui.select.down")) {
				this.move(1);
				return;
			}
			if (keybindings.matches(data, "tui.select.confirm")) {
				this.activateSelected();
				return;
			}
			this.fetchedFilter.handleInput(data);
			return;
		}

		if (keybindings.matches(data, "tui.select.up")) {
			this.move(-1);
			return;
		}
		if (keybindings.matches(data, "tui.select.down")) {
			this.move(1);
			return;
		}
		if (keybindings.matches(data, "tui.select.confirm")) this.activateSelected();
	}

	private items(): FormItem[] {
		const items: FormItem[] = [
			{ kind: "template" },
			{ kind: "id" },
			{ kind: "name" },
			{ kind: "base-url" },
			{ kind: "api" },
			{ kind: "api-key" },
			{ kind: "oauth" },
			{ kind: "clear-auth" },
			{ kind: "models" },
			...(this.draft.provider.models ?? []).map((model) => ({ kind: "model" as const, modelId: model.id })),
		];

		const expandedModel = this.expandedModel();
		if (expandedModel) {
			items.push(
				{ kind: "model-name", modelId: expandedModel.id },
				{ kind: "model-api", modelId: expandedModel.id },
				{ kind: "model-base-url", modelId: expandedModel.id },
				{ kind: "model-reasoning", modelId: expandedModel.id },
				{ kind: "model-input", modelId: expandedModel.id },
				{ kind: "model-context-window", modelId: expandedModel.id },
				{ kind: "model-max-tokens", modelId: expandedModel.id },
				{ kind: "model-advanced", modelId: expandedModel.id },
			);
			if (this.modelAdvancedExpanded) {
				items.push(
					...THINKING_LEVELS.map((level) => ({
						kind: "model-thinking" as const,
						modelId: expandedModel.id,
						level,
					})),
					...COST_RATES.map((rate) => ({ kind: "model-cost" as const, modelId: expandedModel.id, rate })),
					{ kind: "model-headers", modelId: expandedModel.id },
					{ kind: "model-compat", modelId: expandedModel.id },
				);
				if (this.modelHeadersExpanded) {
					items.push(
						...Object.keys(expandedModel.headers ?? {})
							.sort((left, right) => left.localeCompare(right))
							.map((name) => ({ kind: "model-header" as const, modelId: expandedModel.id, name })),
						{ kind: "model-add-header", modelId: expandedModel.id },
					);
				}
				if (this.modelCompatExpanded) {
					items.push(
						...COMMON_COMPAT_BOOLEAN_OPTIONS.map((option) => ({
							kind: "model-compat-boolean" as const,
							modelId: expandedModel.id,
							option,
						})),
						...COMMON_COMPAT_ENUM_OPTIONS.map((option) => ({
							kind: "model-compat-enum" as const,
							modelId: expandedModel.id,
							option,
						})),
					);
				}
			}
			items.push({ kind: "remove-model", modelId: expandedModel.id });
		}

		items.push({ kind: "add-model" }, { kind: "fetch-models" });

		if (this.isFetchedModelPickerOpen()) {
			items.push(
				{ kind: "fetched-filter" },
				...this.filteredFetchedModels().map((model) => ({ kind: "fetched-model" as const, modelId: model.id })),
				{ kind: "add-fetched-models" },
				{ kind: "close-fetched-models" },
			);
		}

		items.push({ kind: "advanced" });
		if (this.advancedExpanded) {
			items.push(
				{ kind: "provider-auth-header" },
				{ kind: "provider-oauth-implementation" },
				{ kind: "provider-headers" },
				{ kind: "provider-compat" },
				{ kind: "provider-overrides" },
			);
			if (this.providerHeadersExpanded) {
				items.push(
					...Object.keys(this.draft.provider.headers ?? {})
						.sort((left, right) => left.localeCompare(right))
						.map((name) => ({ kind: "provider-header" as const, name })),
					{ kind: "provider-add-header" },
				);
			}
			if (this.providerCompatExpanded) {
				items.push(
					...COMMON_COMPAT_BOOLEAN_OPTIONS.map((option) => ({ kind: "provider-compat-boolean" as const, option })),
					...COMMON_COMPAT_ENUM_OPTIONS.map((option) => ({ kind: "provider-compat-enum" as const, option })),
				);
			}
			if (this.overridesExpanded) {
				const overrides = this.draft.provider.modelOverrides ?? {};
				items.push(
					...Object.keys(overrides)
						.sort((left, right) => left.localeCompare(right))
						.map((overrideId) => ({ kind: "model-override" as const, overrideId })),
				);
				const override = this.expandedOverride();
				if (override && this.expandedOverrideId) {
					items.push(
						{ kind: "override-name", overrideId: this.expandedOverrideId },
						{ kind: "override-reasoning", overrideId: this.expandedOverrideId },
						{ kind: "override-input", overrideId: this.expandedOverrideId },
						{ kind: "override-context-window", overrideId: this.expandedOverrideId },
						{ kind: "override-max-tokens", overrideId: this.expandedOverrideId },
						{ kind: "remove-override", overrideId: this.expandedOverrideId },
					);
				}
				items.push({ kind: "add-override" });
			}
		}
		return items;
	}

	private renderItem(item: FormItem, width: number): string[] {
		switch (item.kind) {
			case "template":
				return this.renderWithEditor(item, "Template", this.templateLabel(), width, this.isTemplatePickerOpen());
			case "id":
				return this.renderWithEditor(
					item,
					"Provider ID",
					this.isCustomTemplate() ? this.draft.id || "Not set" : `${this.draft.id} (fixed by template)`,
					width,
					this.editing?.field === "id",
				);
			case "name":
				return this.renderWithEditor(
					item,
					"Name",
					this.draft.provider.name ?? "Not set",
					width,
					this.editing?.field === "name",
				);
			case "base-url":
				return this.renderWithEditor(
					item,
					"Base URL",
					this.draft.provider.baseUrl ?? "Not set",
					width,
					this.editing?.field === "base-url",
				);
			case "api":
				return this.renderProtocolPicker(item, width);
			case "api-key":
				return this.renderWithEditor(item, "API key", this.apiKeyState(), width, this.editing?.field === "api-key");
			case "oauth":
				return [this.row(item, "OAuth", this.oauthState(), width)];
			case "clear-auth":
				return [this.row(item, "Clear authentication", "", width)];
			case "models":
				return [this.row(item, "Models", `${this.draft.provider.models?.length ?? 0} configured`, width)];
			case "model":
				return [
					this.row(
						item,
						`Model · ${item.modelId}`,
						this.expandedModelId === item.modelId ? "Editing" : (this.modelById(item.modelId)?.name ?? "Edit"),
						width,
					),
				];
			case "model-name":
				return this.renderWithEditor(
					item,
					"  Display name",
					this.modelById(item.modelId)?.name ?? "Not set",
					width,
					this.editing?.field === "model-name",
				);
			case "model-api":
				return this.renderWithEditor(
					item,
					"  API override",
					this.modelById(item.modelId)?.api ?? "Inherit provider",
					width,
					this.editing?.field === "model-api",
				);
			case "model-base-url":
				return this.renderWithEditor(
					item,
					"  Base URL override",
					this.modelById(item.modelId)?.baseUrl ?? "Inherit provider",
					width,
					this.editing?.field === "model-base-url",
				);
			case "model-reasoning":
				return [this.row(item, "  Reasoning", booleanState(this.modelById(item.modelId)?.reasoning), width)];
			case "model-input":
				return [
					this.row(
						item,
						"  Input modalities",
						this.modelById(item.modelId)?.input?.join(", ") ?? "Inherit provider",
						width,
					),
				];
			case "model-context-window":
				return this.renderWithEditor(
					item,
					"  Context window",
					numberState(this.modelById(item.modelId)?.contextWindow),
					width,
					this.editing?.field === "model-context-window",
				);
			case "model-max-tokens":
				return this.renderWithEditor(
					item,
					"  Max output tokens",
					numberState(this.modelById(item.modelId)?.maxTokens),
					width,
					this.editing?.field === "model-max-tokens",
				);
			case "model-advanced":
				return [
					this.row(
						item,
						"  Model advanced settings",
						this.modelAdvancedExpanded ? "Expanded" : "Collapsed",
						width,
					),
				];
			case "model-thinking":
				return this.renderWithEditor(
					item,
					`    Thinking · ${item.level}`,
					this.thinkingValue(item.modelId, item.level),
					width,
					this.editing?.field === "model-thinking" && this.editing.level === item.level,
				);
			case "model-cost":
				return this.renderWithEditor(
					item,
					`    Cost · ${item.rate}`,
					this.costValue(item.modelId, item.rate),
					width,
					this.editing?.field === "model-cost" && this.editing.rate === item.rate,
				);
			case "model-headers":
				return [
					this.row(
						item,
						"    Request headers",
						this.modelHeadersExpanded
							? "Expanded"
							: `${Object.keys(this.modelById(item.modelId)?.headers ?? {}).length} configured`,
						width,
					),
				];
			case "model-header":
				return [this.row(item, `      ${item.name}`, "Edit value", width)];
			case "model-add-header":
				return [this.row(item, "      + Add request header", "", width)];
			case "model-compat":
				return [
					this.row(
						item,
						"    Compatibility",
						this.modelCompatExpanded
							? "Expanded"
							: this.modelById(item.modelId)?.compat
								? "Configured"
								: "Default",
						width,
					),
				];
			case "model-compat-boolean":
				return [
					this.row(
						item,
						`      ${item.option.label}`,
						triState(this.modelCompatBoolean(item.modelId, item.option.key)),
						width,
					),
				];
			case "model-compat-enum":
				return [
					this.row(
						item,
						`      ${item.option.label}`,
						this.modelCompatEnum(item.modelId, item.option.key) ?? "Automatic",
						width,
					),
				];
			case "remove-model":
				return [this.row(item, "  Remove this model", "", width)];
			case "add-model":
				return this.renderWithEditor(item, "Add model manually", "", width, this.editing?.field === "add-model");
			case "fetch-models":
				return [
					this.row(
						item,
						"Fetch models",
						this.fetchingModels ? "Loading..." : (this.modelFetchDisabledReason() ?? ""),
						width,
					),
				];
			case "fetched-filter":
				return this.renderFetchedFilter(item, width);
			case "fetched-model":
				return [this.row(item, item.modelId, this.isFetchedModelSelected(item.modelId) ? "Selected" : "", width)];
			case "add-fetched-models":
				return [
					this.row(item, "Add selected fetched models", `${this.selectedFetchedModelIds.size} selected`, width),
				];
			case "close-fetched-models":
				return [this.row(item, "Done fetching models", "", width)];
			case "advanced":
				return [this.row(item, "Advanced settings", this.advancedExpanded ? "Expanded" : "Collapsed", width)];
			case "provider-auth-header":
				return [this.row(item, "  Authorization header", triState(this.draft.provider.authHeader), width)];
			case "provider-oauth-implementation":
				return [this.row(item, "  OAuth implementation", this.draft.provider.oauth ?? "Off", width)];
			case "provider-headers":
				return [
					this.row(
						item,
						"  Shared headers",
						this.providerHeadersExpanded
							? "Expanded"
							: `${Object.keys(this.draft.provider.headers ?? {}).length} configured`,
						width,
					),
				];
			case "provider-header":
				return [this.row(item, `    ${item.name}`, "Edit value", width)];
			case "provider-add-header":
				return [this.row(item, "    + Add shared header", "", width)];
			case "provider-compat":
				return [
					this.row(
						item,
						"  Compatibility",
						this.providerCompatExpanded ? "Expanded" : this.draft.provider.compat ? "Configured" : "Default",
						width,
					),
				];
			case "provider-compat-boolean":
				return [
					this.row(item, `    ${item.option.label}`, triState(this.providerCompatBoolean(item.option.key)), width),
				];
			case "provider-compat-enum":
				return [
					this.row(
						item,
						`    ${item.option.label}`,
						this.providerCompatEnum(item.option.key) ?? "Automatic",
						width,
					),
				];
			case "provider-overrides":
				return [
					this.row(
						item,
						"  Model overrides",
						this.overridesExpanded
							? "Expanded"
							: `${Object.keys(this.draft.provider.modelOverrides ?? {}).length} configured`,
						width,
					),
				];
			case "model-override":
				return [
					this.row(
						item,
						`    Override · ${item.overrideId}`,
						this.expandedOverrideId === item.overrideId ? "Editing" : "Edit",
						width,
					),
				];
			case "override-name":
				return this.renderWithEditor(
					item,
					"      Display name",
					this.overrideById(item.overrideId)?.name ?? "Not set",
					width,
					this.editing?.field === "override-name",
				);
			case "override-reasoning":
				return [this.row(item, "      Reasoning", triState(this.overrideById(item.overrideId)?.reasoning), width)];
			case "override-input":
				return [
					this.row(
						item,
						"      Input modalities",
						this.overrideById(item.overrideId)?.input?.join(", ") ?? "Automatic",
						width,
					),
				];
			case "override-context-window":
				return this.renderWithEditor(
					item,
					"      Context window",
					numberState(this.overrideById(item.overrideId)?.contextWindow),
					width,
					this.editing?.field === "override-context-window",
				);
			case "override-max-tokens":
				return this.renderWithEditor(
					item,
					"      Max output tokens",
					numberState(this.overrideById(item.overrideId)?.maxTokens),
					width,
					this.editing?.field === "override-max-tokens",
				);
			case "remove-override":
				return [this.row(item, "      Remove this override", "", width)];
			case "add-override":
				return this.renderWithEditor(
					item,
					"    + Add model override",
					"",
					width,
					this.editing?.field === "override-id",
				);
		}
	}

	private renderWithEditor(item: FormItem, label: string, value: string, width: number, isEditing: boolean): string[] {
		const lines = [this.row(item, label, value, width)];
		if (isEditing && this.editing) lines.push(...this.editing.input.render(width));
		if (item.kind === "template" && this.isTemplatePickerOpen()) {
			lines.push(...this.templateSearch.render(width));
			const templates = this.filteredTemplates();
			if (templates.length === 0) lines.push(theme.fg("muted", "  No matching templates"));
			for (let index = 0; index < templates.length; index++) {
				const option = templates[index]!;
				const selected = index === this.templatePickerIndex;
				const prefix = selected ? theme.fg("accent", "› ") : "  ";
				lines.push(
					truncateToWidth(`${prefix}${selected ? theme.fg("accent", option.name) : option.name}`, width, ""),
				);
			}
		}
		return lines;
	}

	private renderFetchedFilter(item: FormItem, width: number): string[] {
		const lines = [this.row(item, "Filter fetched models", this.fetchedFilter.getValue() || "All models", width)];
		lines.push(...this.fetchedFilter.render(width));
		return lines;
	}

	private renderProtocolPicker(item: FormItem, width: number): string[] {
		const lines = [this.row(item, "API protocol", this.protocolLabel(), width)];
		if (!this.protocolPickerOpen) return lines;
		lines.push(theme.fg("muted", "  Choose the Provider request format"));
		for (let index = 0; index < API_PROTOCOLS.length; index++) {
			const option = API_PROTOCOLS[index]!;
			const selected = index === this.protocolPickerIndex;
			const prefix = selected ? theme.fg("accent", "› ") : "  ";
			lines.push(
				truncateToWidth(`${prefix}${selected ? theme.fg("accent", option.label) : option.label}`, width, ""),
			);
		}
		return lines;
	}

	private row(item: FormItem, label: string, value: string, width: number): string {
		const selected = this.selectedItemKey() === this.itemKey(item);
		const prefix = selected ? theme.fg("accent", "› ") : "  ";
		const text = `${prefix}${selected ? theme.fg("accent", label) : label}${value ? `  ${theme.fg("muted", value)}` : ""}`;
		return truncateToWidth(text, width, "");
	}

	private activateSelected(): void {
		const item = this.items()[this.selectedIndex];
		if (!item) return;
		switch (item.kind) {
			case "template":
				this.openTemplatePicker();
				break;
			case "id":
				if (this.isCustomTemplate()) this.openTextEditor("id", this.draft.id);
				break;
			case "name":
				this.openTextEditor("name", this.draft.provider.name ?? "");
				break;
			case "base-url":
				this.openTextEditor("base-url", this.draft.provider.baseUrl ?? "");
				break;
			case "api":
				this.openProtocolPicker();
				break;
			case "api-key":
				this.openTextEditor("api-key", "");
				break;
			case "oauth":
				if (this.authentication().oauthAvailable) {
					this.authenticationAction = "oauth";
					void this.props.callbacks.onStageOAuth(this.draft.id);
					this.status = "OAuth sign-in requested";
				} else {
					this.status = "OAuth is not available for this provider";
				}
				break;
			case "clear-auth":
				this.stagedApiKey = undefined;
				this.authenticationAction = "cleared";
				void this.props.callbacks.onClearAuthentication(this.draft.id);
				this.status = "Authentication clear requested";
				break;
			case "models":
				break;
			case "model":
				this.toggleModelEditor(item.modelId);
				break;
			case "model-name":
				this.openTextEditor("model-name", this.modelById(item.modelId)?.name ?? "", item.modelId);
				break;
			case "model-api":
				this.openTextEditor("model-api", this.modelById(item.modelId)?.api ?? "", item.modelId);
				break;
			case "model-base-url":
				this.openTextEditor("model-base-url", this.modelById(item.modelId)?.baseUrl ?? "", item.modelId);
				break;
			case "model-reasoning":
				this.updateModel(item.modelId, (model) => ({ ...model, reasoning: cycleTriState(model.reasoning) }));
				this.status = "Reasoning preference updated";
				break;
			case "model-input":
				this.cycleModelInput(item.modelId);
				break;
			case "model-context-window":
				this.openTextEditor(
					"model-context-window",
					this.modelById(item.modelId)?.contextWindow?.toString() ?? "",
					item.modelId,
				);
				break;
			case "model-max-tokens":
				this.openTextEditor(
					"model-max-tokens",
					this.modelById(item.modelId)?.maxTokens?.toString() ?? "",
					item.modelId,
				);
				break;
			case "model-advanced":
				this.modelAdvancedExpanded = !this.modelAdvancedExpanded;
				this.status = this.modelAdvancedExpanded
					? "Model advanced settings expanded"
					: "Model advanced settings collapsed";
				break;
			case "model-thinking":
				this.openTextEditor(
					"model-thinking",
					this.modelById(item.modelId)?.thinkingLevelMap?.[item.level] ?? "",
					item.modelId,
					item.level,
				);
				break;
			case "model-cost":
				this.openTextEditor(
					"model-cost",
					this.costValue(item.modelId, item.rate),
					item.modelId,
					undefined,
					item.rate,
				);
				break;
			case "model-headers":
				this.modelHeadersExpanded = !this.modelHeadersExpanded;
				this.status = this.modelHeadersExpanded
					? "Model request headers expanded"
					: "Model request headers collapsed";
				break;
			case "model-header":
				this.startHeaderValueEdit({ scope: "model", modelId: item.modelId }, item.name);
				break;
			case "model-add-header":
				this.startHeaderNameEdit({ scope: "model", modelId: item.modelId });
				break;
			case "model-compat":
				this.modelCompatExpanded = !this.modelCompatExpanded;
				this.status = this.modelCompatExpanded
					? "Model compatibility options expanded"
					: "Model compatibility options collapsed";
				break;
			case "model-compat-boolean":
				this.setModelCompat(
					item.modelId,
					item.option.key,
					cycleTriState(this.modelCompatBoolean(item.modelId, item.option.key)),
				);
				this.status = `${item.option.label} updated`;
				break;
			case "model-compat-enum":
				this.cycleModelCompatEnum(item.modelId, item.option);
				break;
			case "remove-model":
				this.removeModel(item.modelId);
				break;
			case "add-model":
				this.openTextEditor("add-model", "");
				break;
			case "fetch-models":
				void this.fetchModels();
				break;
			case "fetched-filter":
				this.fetchedFilter.focused = this.focused;
				break;
			case "fetched-model":
				this.toggleFetchedModel(item.modelId);
				break;
			case "add-fetched-models":
				this.addSelectedFetchedModels();
				break;
			case "close-fetched-models":
				this.closeFetchedModels();
				break;
			case "advanced":
				this.advancedExpanded = !this.advancedExpanded;
				this.status = this.advancedExpanded ? "Advanced settings expanded" : "Advanced settings collapsed";
				break;
			case "provider-auth-header":
				this.draft.provider = { ...this.draft.provider, authHeader: cycleTriState(this.draft.provider.authHeader) };
				this.status = `Authorization header ${triState(this.draft.provider.authHeader).toLowerCase()}`;
				this.commitDraft();
				break;
			case "provider-oauth-implementation":
				this.draft.provider = {
					...this.draft.provider,
					oauth: this.draft.provider.oauth === "radius" ? undefined : "radius",
				};
				this.status = this.draft.provider.oauth ? "OAuth implementation enabled" : "OAuth implementation disabled";
				this.commitDraft();
				break;
			case "provider-headers":
				this.providerHeadersExpanded = !this.providerHeadersExpanded;
				this.status = this.providerHeadersExpanded
					? "Shared request headers expanded"
					: "Shared request headers collapsed";
				break;
			case "provider-header":
				this.startHeaderValueEdit({ scope: "provider" }, item.name);
				break;
			case "provider-add-header":
				this.startHeaderNameEdit({ scope: "provider" });
				break;
			case "provider-compat":
				this.providerCompatExpanded = !this.providerCompatExpanded;
				this.status = this.providerCompatExpanded
					? "Compatibility options expanded"
					: "Compatibility options collapsed";
				break;
			case "provider-compat-boolean":
				this.setProviderCompat(item.option.key, cycleTriState(this.providerCompatBoolean(item.option.key)));
				this.status = `${item.option.label} updated`;
				break;
			case "provider-compat-enum":
				this.cycleProviderCompatEnum(item.option);
				break;
			case "provider-overrides":
				this.overridesExpanded = !this.overridesExpanded;
				if (!this.overridesExpanded) this.expandedOverrideId = undefined;
				this.status = this.overridesExpanded ? "Model overrides expanded" : "Model overrides collapsed";
				break;
			case "model-override":
				this.toggleOverrideEditor(item.overrideId);
				break;
			case "override-name":
				this.openTextEditor(
					"override-name",
					this.overrideById(item.overrideId)?.name ?? "",
					undefined,
					undefined,
					undefined,
				);
				break;
			case "override-reasoning":
				this.updateOverride(item.overrideId, (override) => ({
					...override,
					reasoning: cycleTriState(override.reasoning),
				}));
				this.status = "Override reasoning preference updated";
				break;
			case "override-input":
				this.cycleOverrideInput(item.overrideId);
				break;
			case "override-context-window":
				this.openTextEditor(
					"override-context-window",
					this.overrideById(item.overrideId)?.contextWindow?.toString() ?? "",
				);
				break;
			case "override-max-tokens":
				this.openTextEditor("override-max-tokens", this.overrideById(item.overrideId)?.maxTokens?.toString() ?? "");
				break;
			case "remove-override":
				this.removeOverride(item.overrideId);
				break;
			case "add-override":
				this.openTextEditor("override-id", "");
				break;
		}
	}

	private openTextEditor(
		field: TextField,
		value: string,
		modelId?: string,
		level?: ThinkingLevel,
		rate?: CostRate,
	): void {
		const input = new Input();
		input.setValue(value);
		input.focused = this.focused;
		input.onEscape = () => this.closeEditor();
		input.onSubmit = (next) => this.submitTextEditor(field, next);
		this.editing = { field, input, modelId, level, rate };
	}

	private submitTextEditor(field: TextField, value: string): void {
		const trimmed = value.trim();
		if (field === "header-name") {
			if (!trimmed) {
				this.status = "Enter a header name";
				return;
			}
			if (!this.headerEdit) return;
			this.headerEdit.name = trimmed;
			this.openTextEditor("header-value", "");
			return;
		}
		if (field === "header-value") {
			this.commitHeaderValue(value);
			this.closeEditor();
			return;
		}
		switch (field) {
			case "id":
				this.draft.id = trimmed;
				this.commitDraft();
				break;
			case "name":
				this.setProviderValue("name", trimmed || undefined);
				break;
			case "base-url":
				this.setProviderValue("baseUrl", trimmed || undefined);
				break;
			case "api-key":
				if (trimmed) {
					this.stagedApiKey = trimmed;
					this.authenticationAction = "api_key";
					void this.props.callbacks.onStageApiKey(this.draft.id, trimmed);
					this.status = "API key staged for this provider";
				}
				break;
			case "add-model":
				this.addManualModel(trimmed);
				break;
			case "model-name":
				if (this.editing?.modelId)
					this.updateModel(this.editing.modelId, (model) => ({ ...model, name: trimmed || undefined }));
				break;
			case "model-api":
				if (this.editing?.modelId)
					this.updateModel(this.editing.modelId, (model) => ({ ...model, api: trimmed || undefined }));
				break;
			case "model-base-url":
				if (this.editing?.modelId)
					this.updateModel(this.editing.modelId, (model) => ({ ...model, baseUrl: trimmed || undefined }));
				break;
			case "model-context-window":
				this.setModelPositiveInteger("contextWindow", trimmed);
				break;
			case "model-max-tokens":
				this.setModelPositiveInteger("maxTokens", trimmed);
				break;
			case "model-thinking":
				this.setModelThinkingValue(trimmed);
				break;
			case "model-cost":
				this.setModelCostValue(trimmed);
				break;
			case "override-id":
				this.addOverride(trimmed);
				break;
			case "override-name":
				if (this.expandedOverrideId)
					this.updateOverride(this.expandedOverrideId, (override) => ({
						...override,
						name: trimmed || undefined,
					}));
				break;
			case "override-context-window":
				this.setOverridePositiveInteger("contextWindow", trimmed);
				break;
			case "override-max-tokens":
				this.setOverridePositiveInteger("maxTokens", trimmed);
				break;
		}
		this.closeEditor();
	}

	private setProviderValue<Key extends "name" | "baseUrl">(key: Key, value: ModelsJsonProvider[Key]): void {
		this.draft.provider = { ...this.draft.provider, [key]: value };
		this.commitDraft();
	}

	private startHeaderNameEdit(target: HeaderTarget): void {
		this.headerEdit = { target };
		this.openTextEditor("header-name", "");
	}

	private startHeaderValueEdit(target: HeaderTarget, name: string): void {
		this.headerEdit = { target, name };
		this.openTextEditor("header-value", this.headerValue(target, name));
	}

	private headerValue(target: HeaderTarget, name: string): string {
		return target.scope === "provider"
			? (this.draft.provider.headers?.[name] ?? "")
			: (this.modelById(target.modelId)?.headers?.[name] ?? "");
	}

	private commitHeaderValue(value: string): void {
		const edit = this.headerEdit;
		if (!edit?.name) return;
		const { name, target } = edit;
		const updateHeaders = (headers: Record<string, string> | undefined): Record<string, string> | undefined => {
			const next = { ...(headers ?? {}) };
			if (value) next[name] = value;
			else delete next[name];
			return Object.keys(next).length > 0 ? next : undefined;
		};
		if (target.scope === "provider") {
			this.draft.provider = { ...this.draft.provider, headers: updateHeaders(this.draft.provider.headers) };
		} else {
			this.updateModel(target.modelId, (model) => ({ ...model, headers: updateHeaders(model.headers) }));
		}
		this.headerEdit = undefined;
		this.status = value ? `Header ${name} saved in draft` : `Header ${name} removed from draft`;
		this.commitDraft();
	}

	private providerCompatRecord(): Record<string, unknown> {
		return { ...(this.draft.provider.compat as Record<string, unknown> | undefined) };
	}

	private providerCompatBoolean(key: string): boolean | undefined {
		const value = this.providerCompatRecord()[key];
		return typeof value === "boolean" ? value : undefined;
	}

	private providerCompatEnum(key: string): string | undefined {
		const value = this.providerCompatRecord()[key];
		return typeof value === "string" ? value : undefined;
	}

	private setProviderCompat(key: string, value: boolean | string | undefined): void {
		const compat = this.providerCompatRecord();
		if (value === undefined) delete compat[key];
		else compat[key] = value;
		this.draft.provider = {
			...this.draft.provider,
			compat: Object.keys(compat).length > 0 ? (compat as ModelsJsonProvider["compat"]) : undefined,
		};
		this.commitDraft();
	}

	private cycleProviderCompatEnum(option: CompatEnumOption): void {
		const current = this.providerCompatEnum(option.key);
		const index = current === undefined ? -1 : option.values.indexOf(current);
		const next = index >= option.values.length - 1 ? undefined : option.values[index + 1];
		this.setProviderCompat(option.key, next);
		this.status = `${option.label} ${next ?? "set to automatic"}`;
	}

	private modelCompatRecord(modelId: string): Record<string, unknown> {
		return { ...(this.modelById(modelId)?.compat as Record<string, unknown> | undefined) };
	}

	private modelCompatBoolean(modelId: string, key: string): boolean | undefined {
		const value = this.modelCompatRecord(modelId)[key];
		return typeof value === "boolean" ? value : undefined;
	}

	private modelCompatEnum(modelId: string, key: string): string | undefined {
		const value = this.modelCompatRecord(modelId)[key];
		return typeof value === "string" ? value : undefined;
	}

	private setModelCompat(modelId: string, key: string, value: boolean | string | undefined): void {
		this.updateModel(modelId, (model) => {
			const compat = this.modelCompatRecord(modelId);
			if (value === undefined) delete compat[key];
			else compat[key] = value;
			return {
				...model,
				compat: Object.keys(compat).length > 0 ? (compat as ModelsJsonModel["compat"]) : undefined,
			};
		});
	}

	private cycleModelCompatEnum(modelId: string, option: CompatEnumOption): void {
		const current = this.modelCompatEnum(modelId, option.key);
		const index = current === undefined ? -1 : option.values.indexOf(current);
		const next = index >= option.values.length - 1 ? undefined : option.values[index + 1];
		this.setModelCompat(modelId, option.key, next);
		this.status = `${option.label} ${next ?? "set to automatic"}`;
	}

	private overrideById(overrideId: string): ModelsJsonModelOverride | undefined {
		return this.draft.provider.modelOverrides?.[overrideId];
	}

	private expandedOverride(): ModelsJsonModelOverride | undefined {
		return this.expandedOverrideId ? this.overrideById(this.expandedOverrideId) : undefined;
	}

	private toggleOverrideEditor(overrideId: string): void {
		this.expandedOverrideId = this.expandedOverrideId === overrideId ? undefined : overrideId;
		this.status = this.expandedOverrideId ? `Editing override ${overrideId}` : `Closed override ${overrideId}`;
	}

	private updateOverride(
		overrideId: string,
		update: (override: ModelsJsonModelOverride) => ModelsJsonModelOverride,
	): void {
		const current = this.overrideById(overrideId);
		if (!current) return;
		this.draft.provider = {
			...this.draft.provider,
			modelOverrides: { ...(this.draft.provider.modelOverrides ?? {}), [overrideId]: update(current) },
		};
		this.commitDraft();
	}

	private addOverride(overrideId: string): void {
		if (!overrideId) {
			this.status = "Enter a model ID for the override";
			return;
		}
		if (this.overrideById(overrideId)) {
			this.status = `Override ${overrideId} is already in this draft`;
			return;
		}
		this.draft.provider = {
			...this.draft.provider,
			modelOverrides: { ...(this.draft.provider.modelOverrides ?? {}), [overrideId]: {} },
		};
		this.overridesExpanded = true;
		this.expandedOverrideId = overrideId;
		this.status = `Added override ${overrideId}`;
		this.commitDraft();
	}

	private removeOverride(overrideId: string): void {
		const overrides = { ...(this.draft.provider.modelOverrides ?? {}) };
		delete overrides[overrideId];
		this.draft.provider = {
			...this.draft.provider,
			modelOverrides: Object.keys(overrides).length > 0 ? overrides : undefined,
		};
		if (this.expandedOverrideId === overrideId) this.expandedOverrideId = undefined;
		this.status = `Removed override ${overrideId}`;
		this.commitDraft();
	}

	private cycleOverrideInput(overrideId: string): void {
		this.updateOverride(overrideId, (override) => {
			const current = override.input?.join(",");
			const input: ModelsJsonModelOverride["input"] =
				current === undefined ? ["text"] : current === "text" ? ["text", "image"] : undefined;
			return { ...override, input };
		});
		this.status = "Override input modalities updated";
	}

	private setOverridePositiveInteger(field: "contextWindow" | "maxTokens", value: string): void {
		if (!this.expandedOverrideId) return;
		if (!value) {
			this.updateOverride(this.expandedOverrideId, (override) => ({ ...override, [field]: undefined }));
			return;
		}
		const parsed = Number(value);
		if (!Number.isSafeInteger(parsed) || parsed <= 0) {
			this.status = "Use a positive whole number";
			return;
		}
		this.updateOverride(this.expandedOverrideId, (override) => ({ ...override, [field]: parsed }));
	}

	private modelById(modelId: string): ModelsJsonModel | undefined {
		return this.draft.provider.models?.find((model) => model.id === modelId);
	}

	private expandedModel(): ModelsJsonModel | undefined {
		return this.expandedModelId ? this.modelById(this.expandedModelId) : undefined;
	}

	private toggleModelEditor(modelId: string): void {
		if (this.expandedModelId === modelId) {
			this.expandedModelId = undefined;
			this.modelAdvancedExpanded = false;
			this.modelHeadersExpanded = false;
			this.modelCompatExpanded = false;
			this.status = `Closed model ${modelId}`;
			return;
		}
		this.expandedModelId = modelId;
		this.modelAdvancedExpanded = false;
		this.modelHeadersExpanded = false;
		this.modelCompatExpanded = false;
		this.status = `Editing model ${modelId} within this Provider form`;
	}

	private updateModel(modelId: string, update: (model: ModelsJsonModel) => ModelsJsonModel): void {
		const models = this.draft.provider.models ?? [];
		const index = models.findIndex((model) => model.id === modelId);
		if (index < 0) return;
		const next = [...models];
		next[index] = update(next[index]!);
		this.draft.provider = { ...this.draft.provider, models: next };
		this.commitDraft();
	}

	private cycleModelInput(modelId: string): void {
		this.updateModel(modelId, (model) => {
			const current = model.input?.join(",");
			const input: ModelsJsonModel["input"] =
				current === undefined ? ["text"] : current === "text" ? ["text", "image"] : undefined;
			return { ...model, input };
		});
		this.status = "Input modalities updated";
	}

	private setModelPositiveInteger(field: "contextWindow" | "maxTokens", value: string): void {
		const modelId = this.editing?.modelId;
		if (!modelId) return;
		if (!value) {
			this.updateModel(modelId, (model) => ({ ...model, [field]: undefined }));
			return;
		}
		const parsed = Number(value);
		if (!Number.isSafeInteger(parsed) || parsed <= 0) {
			this.status = "Use a positive whole number";
			return;
		}
		this.updateModel(modelId, (model) => ({ ...model, [field]: parsed }));
	}

	private thinkingValue(modelId: string, level: ThinkingLevel): string {
		const value = this.modelById(modelId)?.thinkingLevelMap?.[level];
		return value === undefined ? "Not set" : value === null ? "Null" : value;
	}

	private setModelThinkingValue(value: string): void {
		const { modelId, level } = this.editing ?? {};
		if (!modelId || !level) return;
		this.updateModel(modelId, (model) => {
			const next = { ...(model.thinkingLevelMap ?? {}) };
			if (!value) delete next[level];
			else next[level] = value === "null" ? null : value;
			return { ...model, thinkingLevelMap: Object.keys(next).length > 0 ? next : undefined };
		});
	}

	private costValue(modelId: string, rate: CostRate): string {
		return this.modelById(modelId)?.cost?.[rate]?.toString() ?? "0";
	}

	private setModelCostValue(value: string): void {
		const { modelId, rate } = this.editing ?? {};
		if (!modelId || !rate) return;
		const parsed = Number(value);
		if (!Number.isFinite(parsed) || parsed < 0) {
			this.status = "Use a non-negative number";
			return;
		}
		this.updateModel(modelId, (model) => {
			const cost = {
				input: model.cost?.input ?? 0,
				output: model.cost?.output ?? 0,
				cacheRead: model.cost?.cacheRead ?? 0,
				cacheWrite: model.cost?.cacheWrite ?? 0,
				...(model.cost?.tiers ? { tiers: structuredClone(model.cost.tiers) } : {}),
				[rate]: parsed,
			};
			return { ...model, cost };
		});
	}

	private addManualModel(id: string): void {
		if (!id) {
			this.status = "Enter a model ID";
			return;
		}
		if ((this.draft.provider.models ?? []).some((model) => model.id === id)) {
			this.status = `Model ${id} is already in this draft`;
			return;
		}
		this.draft.provider = { ...this.draft.provider, models: [...(this.draft.provider.models ?? []), { id }] };
		this.status = `Added model ${id}`;
		this.commitDraft();
		this.selectedIndex = this.items().findIndex((item) => item.kind === "add-model");
	}

	private removeModel(id: string): void {
		this.draft.provider = {
			...this.draft.provider,
			models: (this.draft.provider.models ?? []).filter((model) => model.id !== id),
		};
		this.status = `Removed model ${id}`;
		this.commitDraft();
		if (this.expandedModelId === id) {
			this.expandedModelId = undefined;
			this.modelAdvancedExpanded = false;
			this.modelHeadersExpanded = false;
			this.modelCompatExpanded = false;
		}
		this.selectedIndex = Math.min(this.selectedIndex, this.items().length - 1);
	}

	private async fetchModels(): Promise<void> {
		if (!this.props.callbacks.onFetchModels) {
			this.status = "Model fetching is not available";
			return;
		}
		const disabledReason = this.modelFetchDisabledReason();
		if (disabledReason) {
			this.status = disabledReason;
			return;
		}
		this.fetchingModels = true;
		this.status = "Fetching models...";
		try {
			const fetched = await this.props.callbacks.onFetchModels(this.getDraft(), this.stagedApiKey);
			this.fetchedModels = fetched.map((model) => structuredClone(model));
			this.selectedFetchedModelIds.clear();
			this.fetchedFilter.setValue("");
			this.fetchedModelPickerOpen = true;
			this.fetchedFilter.focused = this.focused;
			this.selectedIndex = this.items().findIndex((item) => item.kind === "fetched-filter");
			this.status = `${this.fetchedModels.length} models fetched`;
		} catch (error) {
			this.status = `Unable to fetch models: ${error instanceof Error ? error.message : String(error)}`;
		} finally {
			this.fetchingModels = false;
		}
	}

	private filteredFetchedModels(): ModelsJsonModel[] {
		const models = this.fetchedModels ?? [];
		const query = this.fetchedFilter.getValue();
		return query ? fuzzyFilter(models, query, (model) => `${model.id} ${model.name ?? ""}`) : models;
	}

	private toggleFetchedModel(id: string): void {
		if (this.selectedFetchedModelIds.has(id)) this.selectedFetchedModelIds.delete(id);
		else this.selectedFetchedModelIds.add(id);
	}

	private addSelectedFetchedModels(): void {
		const existing = new Set((this.draft.provider.models ?? []).map((model) => model.id));
		const additions = (this.fetchedModels ?? []).filter(
			(model) => this.selectedFetchedModelIds.has(model.id) && !existing.has(model.id),
		);
		if (additions.length === 0) {
			this.status = "Select fetched models that are not already in this draft";
			return;
		}
		this.draft.provider = {
			...this.draft.provider,
			models: [...(this.draft.provider.models ?? []), ...additions.map((model) => structuredClone(model))],
		};
		this.status = `Added ${additions.length} fetched model${additions.length === 1 ? "" : "s"}`;
		this.commitDraft();
		this.closeFetchedModels();
	}

	private openTemplatePicker(): void {
		this.templateSearch.setValue("");
		this.templatePickerIndex = 0;
		this.templatePickerOpen = true;
		this.templateSearch.focused = this.focused;
	}

	private closeTemplatePicker(): void {
		this.templatePickerOpen = false;
		this.templateSearch.setValue("");
		this.templateSearch.focused = false;
	}

	private openProtocolPicker(): void {
		const selected = API_PROTOCOLS.findIndex((option) => option.id === this.draft.provider.api);
		this.protocolPickerIndex = selected >= 0 ? selected : 0;
		this.protocolPickerOpen = true;
	}

	private closeProtocolPicker(): void {
		this.protocolPickerOpen = false;
	}

	private moveProtocolPicker(delta: number): void {
		this.protocolPickerIndex = Math.max(0, Math.min(API_PROTOCOLS.length - 1, this.protocolPickerIndex + delta));
	}

	private selectProtocol(): void {
		const option = API_PROTOCOLS[this.protocolPickerIndex];
		if (!option) return;
		this.draft.provider = { ...this.draft.provider, api: option.id };
		this.status = `API protocol set to ${option.label}`;
		this.closeProtocolPicker();
		this.commitDraft();
	}

	private selectTemplate(): void {
		const option = this.filteredTemplates()[this.templatePickerIndex];
		if (!option) return;
		this.templateId = option.id;
		if (option.id !== CUSTOM_TEMPLATE_ID) {
			this.draft = { id: option.id, provider: structuredClone(option.provider) };
		} else if (!this.draft.provider.api) {
			this.draft.provider = customProvider();
		}
		this.status = `${option.name} template selected`;
		this.closeTemplatePicker();
		this.commitDraft();
	}

	private filteredTemplates(): Array<{ id: string; name: string; provider: ModelsJsonProvider }> {
		const options = [
			{ id: CUSTOM_TEMPLATE_ID, name: CUSTOM_TEMPLATE_NAME, provider: customProvider() },
			...this.props.presets
				.filter((preset) => preset.id !== CUSTOM_TEMPLATE_ID)
				.map((preset) => ({
					id: preset.id,
					name: preset.label,
					provider: {
						name: preset.label,
						baseUrl: preset.baseUrl,
						api: preset.api,
						models: [],
					},
				})),
		];
		const query = this.templateSearch.getValue();
		return query ? fuzzyFilter(options, query, (option) => `${option.name} ${option.id}`) : options;
	}

	private moveTemplatePicker(delta: number): void {
		const options = this.filteredTemplates();
		if (options.length === 0) return;
		this.templatePickerIndex = Math.max(0, Math.min(options.length - 1, this.templatePickerIndex + delta));
	}

	private move(delta: number): void {
		const items = this.items();
		if (items.length === 0) return;
		this.selectedIndex = Math.max(0, Math.min(items.length - 1, this.selectedIndex + delta));
	}

	private closeEditor(): void {
		const field = this.editing?.field;
		if (this.editing) this.editing.input.focused = false;
		this.editing = undefined;
		if (field === "header-name" || field === "header-value") this.headerEdit = undefined;
	}

	private closeFetchedModels(): void {
		this.fetchedModelPickerOpen = false;
		this.fetchedModels = undefined;
		this.selectedFetchedModelIds.clear();
		this.fetchedFilter.setValue("");
		this.fetchedFilter.focused = false;
		this.selectedIndex = Math.min(this.selectedIndex, this.items().length - 1);
	}

	private commitDraft(): void {
		this.props.callbacks.onDraftChange?.(this.getDraft());
	}

	private findTemplateId(draft: ProviderFormDraft): string | undefined {
		return this.props.presets.find((preset) => preset.id === draft.id)?.id;
	}

	private templateLabel(): string {
		return this.templateId === CUSTOM_TEMPLATE_ID
			? CUSTOM_TEMPLATE_NAME
			: (this.props.presets.find((preset) => preset.id === this.templateId)?.label ?? this.templateId);
	}

	private protocolLabel(): string {
		const current = API_PROTOCOLS.find((option) => option.id === this.draft.provider.api);
		return current?.label ?? this.draft.provider.api ?? "Not set";
	}

	private isCustomTemplate(): boolean {
		return this.templateId === CUSTOM_TEMPLATE_ID;
	}

	private isTemplatePickerOpen(): boolean {
		return this.templatePickerOpen;
	}

	private isFetchedModelPickerOpen(): boolean {
		return this.fetchedModelPickerOpen;
	}

	private isFetchedModelSelected(id: string): boolean {
		return this.selectedFetchedModelIds.has(id);
	}

	private modelFetchDisabledReason(): string | undefined {
		if (!this.draft.provider.baseUrl?.trim()) return "Enter Base URL first";
		if (this.draft.provider.api !== "openai-completions" && this.draft.provider.api !== "openai-responses") {
			return "OpenAI-compatible API required";
		}
		if ((this.draft.provider.authHeader ?? true) && !this.stagedApiKey && this.authentication().type !== "api_key") {
			return "Enter API Key first";
		}
		return undefined;
	}

	private apiKeyState(): string {
		if (this.stagedApiKey) return "Staged";
		if (this.authenticationAction === "cleared") return "Cleared (staged)";
		return this.authentication().type === "api_key" ? "Configured" : "Not configured";
	}

	private oauthState(): string {
		if (!this.authentication().oauthAvailable) return "Not available";
		if (this.authenticationAction === "oauth") return "Sign-in requested";
		return this.authentication().type === "oauth" ? "Signed in" : "Not signed in";
	}

	private authentication(): ProviderAuthenticationStatus {
		return this.props.getAuthentication?.(this.draft.id) ?? this.props.authentication;
	}

	private selectedItemKey(): string | undefined {
		const selected = this.items()[this.selectedIndex];
		return selected ? this.itemKey(selected) : undefined;
	}

	private itemKey(item: FormItem): string {
		if (item.kind === "model" || item.kind === "fetched-model") return `${item.kind}:${item.modelId}`;
		if (
			item.kind === "model-name" ||
			item.kind === "model-api" ||
			item.kind === "model-base-url" ||
			item.kind === "model-reasoning" ||
			item.kind === "model-input" ||
			item.kind === "model-context-window" ||
			item.kind === "model-max-tokens" ||
			item.kind === "model-advanced" ||
			item.kind === "remove-model"
		)
			return `${item.kind}:${item.modelId}`;
		if (item.kind === "model-thinking") return `${item.kind}:${item.modelId}:${item.level}`;
		if (item.kind === "model-cost") return `${item.kind}:${item.modelId}:${item.rate}`;
		if (item.kind === "model-header") return `${item.kind}:${item.modelId}:${item.name}`;
		if (item.kind === "model-add-header") return `${item.kind}:${item.modelId}`;
		if (item.kind === "model-compat") return `${item.kind}:${item.modelId}`;
		if (item.kind === "model-compat-boolean" || item.kind === "model-compat-enum")
			return `${item.kind}:${item.modelId}:${item.option.key}`;
		if (item.kind === "provider-header") return `${item.kind}:${item.name}`;
		if (item.kind === "provider-compat-boolean" || item.kind === "provider-compat-enum")
			return `${item.kind}:${item.option.key}`;
		if (
			item.kind === "model-override" ||
			item.kind === "override-name" ||
			item.kind === "override-reasoning" ||
			item.kind === "override-input" ||
			item.kind === "override-context-window" ||
			item.kind === "override-max-tokens" ||
			item.kind === "remove-override"
		)
			return `${item.kind}:${item.overrideId}`;
		return item.kind;
	}
}
