import { type Component, getKeybindings, truncateToWidth } from "@frelion/bone-tui";
import type { ModelsJsonModel, ModelsJsonProvider } from "../../../core/model-config.ts";
import { theme } from "../theme/theme.ts";

export type ModelsProvidersEntry = { kind: "provider"; providerId: string } | { kind: "add-provider" };

/** Credential metadata only; secrets remain in auth.json and are never rendered. */
export interface ProviderAuthenticationStatus {
	type?: "api_key" | "oauth";
	oauthAvailable: boolean;
}

/**
 * Provider-first overview for Providers & Models. It deliberately owns only
 * navigation and rendering; mutations stay with SettingsCenter's transaction
 * draft so this view can be reused without exposing credentials.
 */
export class ModelsProvidersBrowser implements Component {
	private entries: ModelsProvidersEntry[] = [];
	private index = 0;
	private selectedKey: string | undefined;
	private providers = new Map<string, ModelsJsonProvider>();
	public onActivate?: (entry: ModelsProvidersEntry) => void;

	setData(providers: Record<string, ModelsJsonProvider>): void {
		this.providers = new Map(Object.entries(providers));
		this.entries = [
			...Object.keys(providers)
				.sort((left, right) => left.localeCompare(right))
				.map((providerId) => ({ kind: "provider" as const, providerId })),
			{ kind: "add-provider" },
		];
		const restored = this.entries.findIndex((entry) => this.key(entry) === this.selectedKey);
		if (restored >= 0) {
			this.index = restored;
		} else {
			this.index = Math.min(this.index, Math.max(0, this.entries.length - 1));
		}
		this.selectedKey = this.entries[this.index] ? this.key(this.entries[this.index]!) : undefined;
	}

	selected(): ModelsProvidersEntry | undefined {
		return this.entries[this.index];
	}

	selectedLine(): number | undefined {
		const selected = this.selected();
		if (!selected) return undefined;
		let line = 1;
		for (const entry of this.entries) {
			if (this.key(entry) === this.key(selected)) return line;
			line += entry.kind === "provider" ? 2 : 1;
		}
		return undefined;
	}

	invalidate(): void {}

	render(width: number): string[] {
		const lines = [theme.fg("muted", "Providers")];
		for (const entry of this.entries) {
			if (entry.kind === "provider") {
				const provider = this.providers.get(entry.providerId)!;
				lines.push(
					this.row(
						entry,
						`${entry.providerId} · ${provider.name ?? entry.providerId} · ${provider.api ?? "API not set"}`,
						width,
					),
				);
				lines.push(
					truncateToWidth(
						theme.fg("dim", `    ${provider.baseUrl ?? "No base URL"} · ${provider.models?.length ?? 0} models`),
						width,
						"",
					),
				);
				continue;
			}
			if (entry.kind === "add-provider") {
				lines.push(this.row(entry, "+ Add provider", width));
			}
		}
		if (this.entries.length === 1) lines.splice(1, 0, theme.fg("muted", "  No custom providers"));
		lines.push("", theme.fg("muted", "Enter opens the selected Provider or add action"));
		return lines;
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (keybindings.matches(data, "tui.select.up")) this.move(-1);
		else if (keybindings.matches(data, "tui.select.down")) this.move(1);
		else if (keybindings.matches(data, "tui.select.confirm")) {
			const selected = this.selected();
			if (selected) this.onActivate?.(selected);
		}
	}

	private move(delta: number): void {
		if (this.entries.length === 0) return;
		this.index = Math.max(0, Math.min(this.entries.length - 1, this.index + delta));
		this.selectedKey = this.key(this.entries[this.index]!);
	}

	private key(entry: ModelsProvidersEntry): string {
		switch (entry.kind) {
			case "provider":
				return `provider:${entry.providerId}`;
			case "add-provider":
				return "add-provider";
		}
	}

	private row(entry: ModelsProvidersEntry, text: string, width: number): string {
		const selected = this.selected() !== undefined && this.key(this.selected()!) === this.key(entry);
		const prefix = selected ? theme.fg("accent", "› ") : "  ";
		const label = selected ? theme.fg("accent", text) : text;
		return truncateToWidth(`${prefix}${label}`, width, "");
	}
}

export type ProviderDetailItem =
	| { kind: "provider-name" }
	| { kind: "provider-base-url" }
	| { kind: "provider-api" }
	| { kind: "provider-auth-header" }
	| { kind: "provider-oauth" }
	| { kind: "provider-api-key" }
	| { kind: "provider-oauth-login" }
	| { kind: "provider-auth-clear" }
	| { kind: "provider-headers" }
	| { kind: "provider-compat" }
	| { kind: "model"; modelId: string }
	| { kind: "add-model" }
	| { kind: "override"; overrideId: string }
	| { kind: "add-override" }
	| { kind: "delete-provider" };

/** Detail screen for one provider. Model IDs are always rendered beneath it. */
export class ProviderSettingsDetail implements Component {
	private readonly providerId: string;
	private provider: ModelsJsonProvider;
	private authentication: ProviderAuthenticationStatus;
	private items: ProviderDetailItem[] = [];
	private index = 0;
	public onActivate?: (item: ProviderDetailItem) => void;
	public onBack?: () => void;

	constructor(providerId: string, provider: ModelsJsonProvider, authentication: ProviderAuthenticationStatus) {
		this.providerId = providerId;
		this.provider = provider;
		this.authentication = authentication;
		this.rebuildItems();
	}

	getProviderId(): string {
		return this.providerId;
	}

	setProvider(provider: ModelsJsonProvider, authentication: ProviderAuthenticationStatus): void {
		this.provider = provider;
		this.authentication = authentication;
		const selected = this.selected();
		this.rebuildItems();
		if (selected) {
			const restored = this.items.findIndex((item) => this.key(item) === this.key(selected));
			if (restored >= 0) this.index = restored;
		}
	}

	selectedLine(): number {
		const selected = this.selected();
		if (!selected) return 4;
		switch (selected.kind) {
			case "provider-name":
				return 4;
			case "provider-base-url":
				return 6;
			case "provider-api":
				return 7;
			case "provider-auth-header":
				return 10;
			case "provider-oauth":
				return 11;
			case "provider-api-key":
				return 12;
			case "provider-oauth-login":
				return 13;
			case "provider-auth-clear":
				return 14;
			case "provider-headers":
				return 17;
			case "provider-compat":
				return 18;
			case "model":
				return 21 + (this.provider.models?.findIndex((model) => model.id === selected.modelId) ?? 0);
			case "add-model":
				return 21 + (this.provider.models?.length ?? 0);
			case "override":
				return (
					24 +
					(this.provider.models?.length ?? 0) +
					Object.keys(this.provider.modelOverrides ?? {})
						.sort((left, right) => left.localeCompare(right))
						.indexOf(selected.overrideId)
				);
			case "add-override":
				return 24 + (this.provider.models?.length ?? 0) + Object.keys(this.provider.modelOverrides ?? {}).length;
			case "delete-provider":
				return 27 + (this.provider.models?.length ?? 0) + Object.keys(this.provider.modelOverrides ?? {}).length;
		}
	}

	invalidate(): void {}

	render(width: number): string[] {
		const lines = [
			theme.bold(theme.fg("text", `Provider · ${this.providerId}`)),
			theme.fg("muted", `${this.provider.name ?? this.providerId} · ${this.provider.baseUrl ?? "No base URL"}`),
			"",
			theme.fg("muted", "Connection"),
			this.row({ kind: "provider-name" }, "Display name", this.provider.name ?? "Not set", width),
			this.readonly("Provider ID", this.providerId, width),
			this.row({ kind: "provider-base-url" }, "Base URL", this.provider.baseUrl ?? "Not set", width),
			this.row({ kind: "provider-api" }, "API protocol", this.provider.api ?? "Not set", width),
			"",
			theme.fg("muted", "Authentication"),
			this.row(
				{ kind: "provider-api-key" },
				"API Key",
				this.authentication.type === "api_key" ? "Configured" : "Not set",
				width,
			),
			this.row(
				{ kind: "provider-oauth-login" },
				"OAuth",
				!this.authentication.oauthAvailable
					? "Not available"
					: this.authentication.type === "oauth"
						? "Signed in"
						: "Not signed in",
				width,
			),
			this.row(
				{ kind: "provider-auth-clear" },
				"Clear authentication…",
				this.authentication.type ? "" : "Nothing stored",
				width,
			),
			"",
			theme.fg("muted", "Shared request settings"),
			this.row(
				{ kind: "provider-auth-header" },
				"Authorization header",
				this.provider.authHeader === undefined ? "Automatic" : this.provider.authHeader ? "On" : "Off",
				width,
			),
			this.row({ kind: "provider-oauth" }, "OAuth implementation", this.provider.oauth ?? "Off", width),
			this.row(
				{ kind: "provider-headers" },
				"Shared headers",
				`${Object.keys(this.provider.headers ?? {}).length} headers`,
				width,
			),
			this.row({ kind: "provider-compat" }, "Compatibility", this.provider.compat ? "Configured" : "Default", width),
			"",
			theme.fg("muted", `Models · ${this.provider.models?.length ?? 0}`),
		];
		for (const model of this.provider.models ?? []) {
			lines.push(
				this.row(
					{ kind: "model", modelId: model.id },
					model.id,
					`${model.name ?? model.id} · ${model.api ?? "inherits provider"}`,
					width,
				),
			);
		}
		lines.push(this.row({ kind: "add-model" }, "+ Add model", "", width));
		const overrides = Object.keys(this.provider.modelOverrides ?? {}).sort((left, right) =>
			left.localeCompare(right),
		);
		lines.push("", theme.fg("muted", `Overrides · ${overrides.length}`));
		for (const overrideId of overrides)
			lines.push(this.row({ kind: "override", overrideId }, `override:${overrideId}`, "", width));
		lines.push(
			this.row({ kind: "add-override" }, "+ Add override", "", width),
			"",
			theme.fg("muted", "Danger zone"),
			this.row({ kind: "delete-provider" }, "Delete provider…", "", width),
			"",
			theme.fg("muted", "↑↓ select · Enter edit/open · Esc back"),
		);
		return lines;
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (keybindings.matches(data, "tui.select.cancel")) this.onBack?.();
		else if (keybindings.matches(data, "tui.select.up")) this.index = Math.max(0, this.index - 1);
		else if (keybindings.matches(data, "tui.select.down"))
			this.index = Math.min(this.items.length - 1, this.index + 1);
		else if (keybindings.matches(data, "tui.select.confirm")) {
			const selected = this.selected();
			if (selected) this.onActivate?.(selected);
		}
	}

	private rebuildItems(): void {
		this.items = [
			{ kind: "provider-name" },
			{ kind: "provider-base-url" },
			{ kind: "provider-api" },
			{ kind: "provider-auth-header" },
			{ kind: "provider-api-key" },
			{ kind: "provider-oauth-login" },
			{ kind: "provider-auth-clear" },
			{ kind: "provider-oauth" },
			{ kind: "provider-headers" },
			{ kind: "provider-compat" },
			...(this.provider.models ?? []).map((model) => ({ kind: "model" as const, modelId: model.id })),
			{ kind: "add-model" },
			...Object.keys(this.provider.modelOverrides ?? {})
				.sort((left, right) => left.localeCompare(right))
				.map((overrideId) => ({ kind: "override" as const, overrideId })),
			{ kind: "add-override" },
			{ kind: "delete-provider" },
		];
		this.index = Math.min(this.index, Math.max(0, this.items.length - 1));
	}

	private selected(): ProviderDetailItem | undefined {
		return this.items[this.index];
	}

	private key(item: ProviderDetailItem): string {
		return item.kind === "model"
			? `model:${item.modelId}`
			: item.kind === "override"
				? `override:${item.overrideId}`
				: item.kind;
	}

	private readonly(label: string, value: string, width: number): string {
		return truncateToWidth(`  ${label}  ${theme.fg("muted", value)}`, width, "");
	}

	private row(item: ProviderDetailItem, label: string, value: string, width: number): string {
		const selected = this.selected() !== undefined && this.key(this.selected()!) === this.key(item);
		const prefix = selected ? theme.fg("accent", "› ") : "  ";
		const text = `${prefix}${selected ? theme.fg("accent", label) : label}${value ? `  ${theme.fg("muted", value)}` : ""}`;
		return truncateToWidth(text, width, "");
	}
}

export type ModelDetailItem =
	| { kind: "model-name" }
	| { kind: "model-api" }
	| { kind: "model-base-url" }
	| { kind: "model-headers" }
	| { kind: "model-compat" }
	| { kind: "model-reasoning" }
	| { kind: "model-input" }
	| { kind: "model-context-window" }
	| { kind: "model-max-tokens" }
	| { kind: "model-thinking-cost" }
	| { kind: "delete-model" };

/** Detail screen for a concrete remote model under one Provider. */
export class ModelSettingsDetail implements Component {
	private readonly providerId: string;
	private readonly modelId: string;
	private provider: ModelsJsonProvider;
	private model: ModelsJsonModel;
	private readonly items: ModelDetailItem[] = [
		{ kind: "model-name" },
		{ kind: "model-api" },
		{ kind: "model-base-url" },
		{ kind: "model-headers" },
		{ kind: "model-compat" },
		{ kind: "model-reasoning" },
		{ kind: "model-input" },
		{ kind: "model-context-window" },
		{ kind: "model-max-tokens" },
		{ kind: "model-thinking-cost" },
		{ kind: "delete-model" },
	];
	private index = 0;
	public onActivate?: (item: ModelDetailItem) => void;
	public onBack?: () => void;

	constructor(providerId: string, provider: ModelsJsonProvider, model: ModelsJsonModel) {
		this.providerId = providerId;
		this.provider = provider;
		this.model = model;
		this.modelId = model.id;
	}

	getProviderId(): string {
		return this.providerId;
	}

	getModelId(): string {
		return this.modelId;
	}

	setModel(provider: ModelsJsonProvider, model: ModelsJsonModel): void {
		this.provider = provider;
		this.model = model;
	}

	selectedLine(): number {
		return [5, 8, 9, 10, 11, 14, 15, 16, 17, 20, 23][this.index] ?? 5;
	}

	invalidate(): void {}

	render(width: number): string[] {
		const inherit = (value: string | undefined, providerValue: string | undefined) =>
			value ?? `Inherit Provider${providerValue ? ` (${providerValue})` : ""}`;
		return [
			theme.bold(theme.fg("text", `Model · ${this.modelId}`)),
			theme.fg("muted", `via Provider: ${this.providerId} · ${this.provider.baseUrl ?? "No base URL"}`),
			"",
			theme.fg("muted", "Identity"),
			this.readonly("Remote model ID", this.modelId, width),
			this.row({ kind: "model-name" }, "Display name", this.model.name ?? "Not set", width),
			"",
			theme.fg("muted", "Request behavior"),
			this.row({ kind: "model-api" }, "API protocol", inherit(this.model.api, this.provider.api), width),
			this.row({ kind: "model-base-url" }, "Base URL", inherit(this.model.baseUrl, this.provider.baseUrl), width),
			this.row(
				{ kind: "model-headers" },
				"Headers",
				this.model.headers ? `${Object.keys(this.model.headers).length} model headers` : "Inherit Provider",
				width,
			),
			this.row(
				{ kind: "model-compat" },
				"Compatibility",
				this.model.compat ? "Configured" : "Inherit Provider",
				width,
			),
			"",
			theme.fg("muted", "Capabilities & limits"),
			this.row(
				{ kind: "model-reasoning" },
				"Reasoning",
				this.model.reasoning === undefined ? "Inherit Provider" : this.model.reasoning ? "On" : "Off",
				width,
			),
			this.row({ kind: "model-input" }, "Input", this.model.input?.join(", ") ?? "Provider default", width),
			this.row(
				{ kind: "model-context-window" },
				"Context window",
				this.model.contextWindow ? String(this.model.contextWindow) : "Not set",
				width,
			),
			this.row(
				{ kind: "model-max-tokens" },
				"Max output tokens",
				this.model.maxTokens ? String(this.model.maxTokens) : "Not set",
				width,
			),
			"",
			theme.fg("muted", "Advanced"),
			this.row(
				{ kind: "model-thinking-cost" },
				"Thinking & cost",
				this.model.cost || this.model.thinkingLevelMap ? "Configured" : "Provider default",
				width,
			),
			"",
			theme.fg("muted", "Danger zone"),
			this.row({ kind: "delete-model" }, "Delete model…", "", width),
			"",
			theme.fg("muted", "↑↓ select · Enter edit · Esc back"),
		];
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (keybindings.matches(data, "tui.select.cancel")) this.onBack?.();
		else if (keybindings.matches(data, "tui.select.up")) this.index = Math.max(0, this.index - 1);
		else if (keybindings.matches(data, "tui.select.down"))
			this.index = Math.min(this.items.length - 1, this.index + 1);
		else if (keybindings.matches(data, "tui.select.confirm")) this.onActivate?.(this.items[this.index]!);
	}

	private readonly(label: string, value: string, width: number): string {
		return truncateToWidth(`  ${label}  ${theme.fg("muted", value)}`, width, "");
	}

	private row(item: ModelDetailItem, label: string, value: string, width: number): string {
		const selected = this.items[this.index]?.kind === item.kind;
		const prefix = selected ? theme.fg("accent", "› ") : "  ";
		const text = `${prefix}${selected ? theme.fg("accent", label) : label}${value ? `  ${theme.fg("muted", value)}` : ""}`;
		return truncateToWidth(text, width, "");
	}
}
