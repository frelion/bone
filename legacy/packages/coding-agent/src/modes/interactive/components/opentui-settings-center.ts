import { join } from "node:path";
import type { Credential } from "@frelion/bone-ai";
import { CONFIG_DIR_NAME } from "../../../config.ts";
import type { AgentSessionRuntime } from "../../../core/agent-session-runtime.ts";
import type { ExtensionUISelectRequest, ExtensionUIV2Context } from "../../../core/extensions/ui-v2.ts";
import {
	type ForgeConfig,
	type ForgeInstanceConfig,
	loadForgeConfig,
	saveForgeConfig,
} from "../../../core/forge/config.ts";
import { ForgeCredentialStore } from "../../../core/forge/credential-store.ts";
import { ModelConfig, type ModelsJson, type ModelsJsonProvider } from "../../../core/model-config.ts";
import type { Settings, SettingsScope } from "../../../core/settings-manager.ts";
import { SettingsTransactionJournal } from "../../../core/settings-transaction-journal.ts";

type Dialogs = ExtensionUIV2Context["dialogs"];
type ValueKind = "string" | "number" | "boolean";

const SAVE_SETTINGS = "__save_settings__";

export class OpenTUISettingsSaveRequested extends Error {}

interface Field {
	label: string;
	path: string;
	kind: ValueKind;
	choices?: readonly string[];
	scope?: SettingsScope;
	description?: string;
}

const FIELDS: Readonly<Record<string, readonly Field[]>> = {
	"Defaults & Sessions": [
		{ label: "Default provider", path: "defaultProvider", kind: "string" },
		{ label: "Default model", path: "defaultModel", kind: "string" },
		{
			label: "Default thinking level",
			path: "defaultThinkingLevel",
			kind: "string",
			choices: ["off", "minimal", "low", "medium", "high", "xhigh", "max"],
		},
		{ label: "Steering mode", path: "steeringMode", kind: "string", choices: ["all", "one-at-a-time"] },
		{ label: "Follow-up mode", path: "followUpMode", kind: "string", choices: ["all", "one-at-a-time"] },
		{ label: "Session directory", path: "sessionDir", kind: "string" },
		{ label: "Double escape action", path: "doubleEscapeAction", kind: "string", choices: ["fork", "tree", "none"] },
		{
			label: "Tree filter mode",
			path: "treeFilterMode",
			kind: "string",
			choices: ["default", "no-tools", "user-only", "labeled-only", "all"],
		},
	],
	"Context & Delivery": [
		{ label: "Auto compact", path: "compaction.enabled", kind: "boolean" },
		{ label: "Compact reserve tokens", path: "compaction.reserveTokens", kind: "number" },
		{ label: "Compact recent tokens", path: "compaction.keepRecentTokens", kind: "number" },
		{ label: "Branch summary reserve", path: "branchSummary.reserveTokens", kind: "number" },
		{ label: "Skip branch summary prompt", path: "branchSummary.skipPrompt", kind: "boolean" },
		{ label: "Hide thinking block", path: "hideThinkingBlock", kind: "boolean" },
		{ label: "Show cache miss notices", path: "showCacheMissNotices", kind: "boolean" },
		{ label: "Transport", path: "transport", kind: "string", choices: ["auto", "sse", "websocket"] },
		{ label: "Retry enabled", path: "retry.enabled", kind: "boolean" },
		{ label: "Retry attempts", path: "retry.maxRetries", kind: "number" },
		{ label: "Retry base delay", path: "retry.baseDelayMs", kind: "number" },
		{ label: "Provider timeout", path: "retry.provider.timeoutMs", kind: "number" },
	],
	"Appearance & Terminal": [
		{ label: "Theme", path: "theme", kind: "string" },
		{ label: "Show images", path: "terminal.showImages", kind: "boolean" },
		{ label: "Image width cells", path: "terminal.imageWidthCells", kind: "number" },
		{ label: "Auto-resize images", path: "images.autoResize", kind: "boolean" },
		{ label: "Block images", path: "images.blockImages", kind: "boolean" },
		{ label: "Editor padding", path: "editorPaddingX", kind: "number" },
		{ label: "Output padding", path: "outputPad", kind: "number" },
		{ label: "Autocomplete visible", path: "autocompleteMaxVisible", kind: "number" },
		{ label: "Hardware cursor", path: "showHardwareCursor", kind: "boolean" },
		{ label: "Clear on shrink", path: "terminal.clearOnShrink", kind: "boolean" },
		{ label: "Terminal progress", path: "terminal.showTerminalProgress", kind: "boolean" },
		{ label: "Markdown code indent", path: "markdown.codeBlockIndent", kind: "string" },
	],
	"Tools, Shell & Network": [
		{ label: "Enable skill commands", path: "enableSkillCommands", kind: "boolean" },
		{ label: "External editor", path: "externalEditor", kind: "string" },
		{ label: "Shell path", path: "shellPath", kind: "string" },
		{ label: "Shell command prefix", path: "shellCommandPrefix", kind: "string" },
		{ label: "HTTP proxy", path: "httpProxy", kind: "string" },
		{ label: "Provider stream idle timeout", path: "httpIdleTimeoutMs", kind: "number" },
		{ label: "WebSocket connect timeout", path: "websocketConnectTimeoutMs", kind: "number" },
	],
	Resources: [
		{ label: "Skill paths", path: "skills", kind: "string", description: "Comma-separated paths" },
		{ label: "Prompt template paths", path: "prompts", kind: "string", description: "Comma-separated paths" },
		{ label: "Theme paths", path: "themes", kind: "string", description: "Comma-separated paths" },
	],
	"Security & Data": [
		{ label: "Quiet startup", path: "quietStartup", kind: "boolean" },
		{ label: "Collapse changelog", path: "collapseChangelog", kind: "boolean" },
		{ label: "Install telemetry", path: "enableInstallTelemetry", kind: "boolean" },
		{ label: "Analytics", path: "enableAnalytics", kind: "boolean" },
		{ label: "Default project trust", path: "defaultProjectTrust", kind: "string" },
	],
};

function readPath(root: unknown, path: string): unknown {
	let value = root;
	for (const part of path.split(".")) {
		if (typeof value !== "object" || value === null) return undefined;
		value = (value as Record<string, unknown>)[part];
	}
	return value;
}

function writePath(root: Record<string, unknown>, path: string, value: unknown): void {
	const parts = path.split(".");
	let target = root;
	for (const part of parts.slice(0, -1)) {
		const next = target[part];
		if (typeof next !== "object" || next === null || Array.isArray(next)) target[part] = {};
		target = target[part] as Record<string, unknown>;
	}
	const last = parts[parts.length - 1];
	if (value === "" || value === undefined) delete target[last];
	else target[last] = value;
}

function display(value: unknown): string {
	if (Array.isArray(value)) return value.join(", ");
	if (value === undefined || value === null) return "Not set";
	return String(value);
}

function parseValue(value: string, kind: ValueKind): unknown {
	if (kind === "boolean") return value === "true";
	if (kind === "number") {
		const number = Number(value);
		if (!Number.isFinite(number)) throw new Error("Enter a valid number");
		return number;
	}
	if (value.includes(","))
		return value
			.split(",")
			.map((item) => item.trim())
			.filter(Boolean);
	return value;
}

export interface OpenTUISettingsCenterOptions {
	runtime: AgentSessionRuntime;
	ui: ExtensionUIV2Context;
	dialogs?: Dialogs;
	onLogin?: (providerId: string) => Promise<void>;
	onLogout?: (providerId: string) => Promise<void>;
	onPresentationChanged?: () => void | Promise<void>;
	status: (message: string, tone?: "info" | "warning" | "error") => void;
}

/** Native, transactional settings center. Dialogs are deliberately thin; all state lives in this draft. */
export class OpenTUISettingsCenter {
	private readonly runtime: AgentSessionRuntime;
	private readonly dialogs: Dialogs;
	private readonly options: OpenTUISettingsCenterOptions;
	private scope: SettingsScope = "global";
	private global: Settings;
	private project: Settings;
	private models: ModelsJson;
	private forge: ForgeConfig;
	private readonly credentials = new Map<string, string | undefined>();
	private readonly forgeCredentials = new Map<string, string | undefined>();

	constructor(options: OpenTUISettingsCenterOptions) {
		this.options = options;
		this.runtime = options.runtime;
		this.dialogs = options.dialogs ?? options.ui.dialogs;
		const manager = options.runtime.services.settingsManager;
		this.global = manager.getGlobalSettings();
		this.project = manager.getProjectSettings();
		this.models = options.runtime.session.modelRuntime.getModelsJson();
		this.forge = loadForgeConfig(options.runtime.services.agentDir);
	}

	async run(): Promise<void> {
		try {
			for (;;) {
				const page = await this.select({
					title: `Settings · ${this.scope === "global" ? "Global" : "Project"}`,
					options: [
						{
							value: "providers",
							label: "Providers & Models",
							description: "Connections, auth, models and overrides",
						},
						{
							value: "forge",
							label: "GitLab & GitHub",
							description: "Forge hosts, private network and credentials",
						},
						...Object.keys(FIELDS).map((value) => ({ value, label: value })),
						{ value: "scope", label: "Configuration scope", description: "Switch Global / Project" },
						{ value: "global-json", label: "Advanced: global JSON", description: "Edit every global setting" },
						{ value: "project-json", label: "Advanced: project JSON", description: "Edit every project setting" },
						{
							value: "models-json",
							label: "Advanced: models JSON",
							description: "Edit every provider/model field",
						},
					],
				});
				if (!page) return;
				if (page === "scope") await this.switchScope();
				else if (page === "providers") await this.providers();
				else if (page === "forge") await this.forgeSettings();
				else if (page === "global-json" || page === "project-json" || page === "models-json")
					await this.editRaw(page);
				else await this.editPage(page);
			}
		} catch (error) {
			if (!(error instanceof OpenTUISettingsSaveRequested)) throw error;
			await this.save();
		}
	}

	private async select<Value extends string>(request: ExtensionUISelectRequest<Value>): Promise<Value | undefined> {
		const selected = await this.dialogs.select<Value | typeof SAVE_SETTINGS>({
			...request,
			shortcuts: [{ action: "save", value: SAVE_SETTINGS }],
			footer: "Ctrl+S save · Esc discard",
		});
		if (selected === SAVE_SETTINGS) throw new OpenTUISettingsSaveRequested();
		return selected;
	}

	private current(): Settings {
		return this.scope === "global" ? this.global : this.project;
	}

	private async switchScope(): Promise<void> {
		if (!this.runtime.services.settingsManager.isProjectTrusted()) {
			this.options.status("Project settings require a trusted workspace", "warning");
			return;
		}
		const selected = await this.select({
			title: "Configuration scope",
			options: [
				{ value: "global", label: "Global", description: "~/.bone/agent/settings.json" },
				{ value: "project", label: "Project", description: ".bone/settings.json" },
			],
			initialValue: this.scope,
		});
		if (selected) this.scope = selected as SettingsScope;
	}

	private async editPage(page: string): Promise<void> {
		const fields = FIELDS[page];
		if (!fields) return;
		for (;;) {
			const draft = this.current();
			const field = await this.select({
				title: page,
				options: [
					...fields.map((entry) => ({
						value: entry.path,
						label: entry.label,
						description: display(readPath(draft, entry.path)),
					})),
					{ value: "back", label: "Back" },
				],
			});
			if (!field || field === "back") return;
			const entry = fields.find((candidate) => candidate.path === field);
			if (!entry) continue;
			const initial = readPath(draft, entry.path);
			const value =
				entry.kind === "boolean" || entry.choices
					? await this.select({
							title: entry.label,
							options: (entry.choices ?? ["true", "false"]).map((choice) => ({ value: choice, label: choice })),
							initialValue: display(initial === undefined ? (entry.kind === "boolean" ? "false" : "") : initial),
						})
					: await this.dialogs.input({
							title: entry.label,
							placeholder: entry.description ?? display(initial),
							initialValue: display(initial === undefined ? "" : initial),
						});
			if (value === undefined) continue;
			try {
				writePath(this.current() as unknown as Record<string, unknown>, entry.path, parseValue(value, entry.kind));
			} catch (error) {
				this.options.status(error instanceof Error ? error.message : String(error), "error");
			}
		}
	}

	private async editRaw(page: "global-json" | "project-json" | "models-json"): Promise<void> {
		const current = page === "global-json" ? this.global : page === "project-json" ? this.project : this.models;
		const value = await this.dialogs.input({
			title:
				page === "models-json"
					? "Advanced: models JSON"
					: `Advanced: ${page === "global-json" ? "global" : "project"} JSON`,
			multiline: true,
			initialValue: JSON.stringify(current, null, 2),
		});
		if (value === undefined) return;
		try {
			const parsed: unknown = JSON.parse(value);
			if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
				throw new Error("JSON root must be an object");
			if (page === "models-json") {
				const models = parsed as ModelsJson;
				ModelConfig.validate(models);
				this.models = structuredClone(models);
			} else if (page === "global-json") this.global = parsed as Settings;
			else this.project = parsed as Settings;
		} catch (error) {
			this.options.status(
				`Invalid settings JSON: ${error instanceof Error ? error.message : String(error)}`,
				"error",
			);
		}
	}

	private async providers(): Promise<void> {
		for (;;) {
			const providerIds = Object.keys(this.models.providers);
			const providerId = await this.select({
				title: "Providers & Models",
				options: [
					...providerIds.map((id) => ({
						value: id,
						label: id,
						description: `${this.models.providers[id]?.models?.length ?? 0} models`,
					})),
					{ value: "add", label: "Add provider" },
					{ value: "back", label: "Back" },
				],
			});
			if (!providerId || providerId === "back") return;
			if (providerId === "add") await this.addProvider();
			else await this.editProvider(providerId);
		}
	}

	private async forgeSettings(): Promise<void> {
		for (;;) {
			const selected = await this.select({
				title: "GitLab & GitHub",
				options: [
					...this.forge.instances.map((instance) => ({
						value: `${instance.provider}:${instance.host}`,
						label: `${instance.provider} · ${instance.host}`,
						description: instance.apiBaseUrl,
					})),
					{ value: "add", label: "Add forge instance" },
					{ value: "back", label: "Back" },
				],
			});
			if (!selected || selected === "back") return;
			if (selected === "add") await this.addForgeInstance();
			else {
				const instance = this.forge.instances.find((entry) => `${entry.provider}:${entry.host}` === selected);
				if (instance) await this.editForgeInstance(instance);
			}
		}
	}

	private async addForgeInstance(): Promise<void> {
		const provider = await this.select({
			title: "Forge provider",
			options: [
				{ value: "github", label: "GitHub" },
				{ value: "gitlab", label: "GitLab" },
			],
		});
		const host = await this.dialogs.input({
			title: "Host",
			placeholder: provider === "github" ? "github.com" : "gitlab.example.com",
		});
		const apiBaseUrl = await this.dialogs.input({ title: "HTTPS API base URL" });
		if (!provider || !host?.trim() || !apiBaseUrl?.trim()) return;
		const credential = await this.dialogs.input({ title: "Credential key", placeholder: "default" });
		const token = credential ? await this.dialogs.input({ title: "Token or $ENV_VAR reference" }) : undefined;
		const instance: ForgeInstanceConfig = {
			provider: provider as ForgeInstanceConfig["provider"],
			host: host.trim().toLowerCase(),
			apiBaseUrl: apiBaseUrl.trim(),
			credential: credential?.trim() || undefined,
			allowPrivateNetwork: false,
		};
		this.forge = { instances: [...this.forge.instances, instance] };
		if (credential && token !== undefined) this.forgeCredentials.set(credential, token || undefined);
	}

	private async editForgeInstance(instance: ForgeInstanceConfig): Promise<void> {
		for (;;) {
			const action = await this.select({
				title: `${instance.provider} · ${instance.host}`,
				options: [
					{ value: "host", label: "Host", description: instance.host },
					{ value: "api", label: "API base URL", description: instance.apiBaseUrl },
					{ value: "credential", label: "Credential key", description: instance.credential ?? "Not set" },
					{
						value: "network",
						label: "Private network",
						description: instance.allowPrivateNetwork ? "Allowed" : "Blocked",
					},
					{ value: "token", label: "Token or $ENV_VAR" },
					{ value: "delete", label: "Delete instance" },
					{ value: "back", label: "Back" },
				],
			});
			if (!action || action === "back") return;
			if (action === "network") {
				const value = await this.select({
					title: "Private network",
					options: [
						{ value: "false", label: "Public network only" },
						{ value: "true", label: "Allow private network" },
					],
					initialValue: instance.allowPrivateNetwork ? "true" : "false",
				});
				if (value) instance.allowPrivateNetwork = value === "true";
			} else if (action === "token" && instance.credential) {
				const token = await this.dialogs.input({
					title: "Token or $ENV_VAR reference",
					placeholder: "Leave empty to clear",
				});
				if (token !== undefined) this.forgeCredentials.set(instance.credential, token || undefined);
			} else if (action === "delete") {
				const confirmed = await this.dialogs.confirm({
					title: "Delete Forge instance",
					message: `Delete ${instance.provider}:${instance.host}?`,
				});
				if (confirmed) {
					this.forge = { instances: this.forge.instances.filter((entry) => entry !== instance) };
					return;
				}
			} else {
				const value = await this.dialogs.input({
					title: action === "host" ? "Host" : action === "api" ? "API base URL" : "Credential key",
					initialValue:
						action === "host"
							? instance.host
							: action === "api"
								? instance.apiBaseUrl
								: (instance.credential ?? ""),
				});
				if (value === undefined) continue;
				if (action === "host") instance.host = value.trim().toLowerCase();
				else if (action === "api") instance.apiBaseUrl = value.trim();
				else instance.credential = value.trim() || undefined;
			}
		}
	}

	private async addProvider(): Promise<void> {
		const id = await this.dialogs.input({ title: "Provider id", placeholder: "my-provider" });
		if (!id?.trim() || this.models.providers[id.trim()]) return;
		const api = await this.dialogs.input({ title: "API protocol", initialValue: "openai-completions" });
		const baseUrl = await this.dialogs.input({ title: "Base URL", placeholder: "https://api.example.com/v1" });
		const key = await this.dialogs.input({ title: "API key", placeholder: "optional" });
		this.models.providers[id.trim()] = {
			api: api?.trim() || "openai-completions",
			baseUrl: baseUrl?.trim() || undefined,
			models: [],
		};
		if (key) this.credentials.set(id.trim(), key);
	}

	private async editProvider(providerId: string): Promise<void> {
		const provider = this.models.providers[providerId];
		if (!provider) return;
		for (;;) {
			const action = await this.select({
				title: provider.name || providerId,
				options: [
					{ value: "name", label: "Display name", description: display(provider.name) },
					{ value: "baseUrl", label: "Base URL", description: display(provider.baseUrl) },
					{ value: "api", label: "API protocol", description: display(provider.api) },
					{
						value: "key",
						label: "API key",
						description: this.credentials.has(providerId) ? "Staged" : "Stored or environment",
					},
					{ value: "login", label: "Sign in", description: "OAuth or API-key authentication" },
					{ value: "logout", label: "Sign out", description: "Clear provider authentication" },
					{ value: "model", label: "Models" },
					{
						value: "advanced",
						label: "Advanced provider JSON",
						description: "Headers, compatibility and overrides",
					},
					{ value: "delete", label: "Delete provider" },
					{ value: "back", label: "Back" },
				],
			});
			if (!action || action === "back") return;
			if (action === "model") await this.editModels(providerId, provider);
			else if (action === "login") await this.options.onLogin?.(providerId);
			else if (action === "logout") await this.options.onLogout?.(providerId);
			else if (action === "advanced") {
				await this.editProviderJson(providerId, provider);
				return;
			} else if (action === "delete") {
				const confirmed = await this.dialogs.confirm({
					title: "Delete provider",
					message: `Delete ${providerId} and its model configuration?`,
				});
				if (confirmed) {
					delete this.models.providers[providerId];
					this.credentials.set(providerId, undefined);
					return;
				}
			} else if (action === "key") {
				const key = await this.dialogs.input({ title: "API key", placeholder: "Leave empty to clear" });
				if (key !== undefined) this.credentials.set(providerId, key || undefined);
			} else {
				const value = await this.dialogs.input({
					title: action,
					initialValue: display(provider[action as "name" | "baseUrl" | "api"]),
				});
				if (value !== undefined) (provider as Record<string, unknown>)[action] = value || undefined;
			}
		}
	}

	private async editProviderJson(providerId: string, provider: ModelsJsonProvider): Promise<void> {
		const value = await this.dialogs.input({
			title: `Advanced provider JSON · ${providerId}`,
			multiline: true,
			initialValue: JSON.stringify(provider, null, 2),
		});
		if (value === undefined) return;
		try {
			const next = structuredClone(this.models);
			next.providers[providerId] = JSON.parse(value) as ModelsJsonProvider;
			ModelConfig.validate(next);
			this.models = next;
		} catch (error) {
			this.options.status(
				`Invalid provider JSON: ${error instanceof Error ? error.message : String(error)}`,
				"error",
			);
		}
	}

	private async editModels(providerId: string, provider: ModelsJsonProvider): Promise<void> {
		for (;;) {
			let models = provider.models;
			if (!models) {
				models = [];
				provider.models = models;
			}
			const selected = await this.select({
				title: `${providerId} models`,
				options: [
					...models.map((model) => ({
						value: model.id,
						label: model.name || model.id,
						description: model.api || "default API",
					})),
					{ value: "add", label: "Add model" },
					{ value: "back", label: "Back" },
				],
			});
			if (!selected || selected === "back") return;
			if (selected === "add") {
				const id = await this.dialogs.input({ title: "Model id" });
				if (id?.trim()) models.push({ id: id.trim(), name: id.trim(), api: provider.api });
				continue;
			}
			const model = models.find((candidate) => candidate.id === selected);
			if (!model) continue;
			const action = await this.select({
				title: model.name || model.id,
				options: [
					{ value: "name", label: "Display name", description: model.name ?? model.id },
					{ value: "reasoning", label: "Reasoning", description: model.reasoning ? "Enabled" : "Disabled" },
					{
						value: "advanced",
						label: "Advanced model JSON",
						description: "API, headers, thinking, costs and compatibility",
					},
					{ value: "delete", label: "Delete model" },
					{ value: "back", label: "Back" },
				],
			});
			if (action === "name") {
				const name = await this.dialogs.input({ title: "Model name", initialValue: model.name ?? model.id });
				if (name !== undefined) model.name = name || undefined;
			} else if (action === "reasoning") model.reasoning = !model.reasoning;
			else if (action === "delete") {
				const confirmed = await this.dialogs.confirm({
					title: "Delete model",
					message: `Delete ${providerId}/${model.id}?`,
				});
				if (confirmed) models.splice(models.indexOf(model), 1);
			} else if (action === "advanced") {
				await this.editModelJson(providerId, models.indexOf(model), model);
				return;
			}
		}
	}

	private async editModelJson(
		providerId: string,
		index: number,
		model: NonNullable<ModelsJsonProvider["models"]>[number],
	): Promise<void> {
		const value = await this.dialogs.input({
			title: `Advanced model JSON · ${model.id}`,
			multiline: true,
			initialValue: JSON.stringify(model, null, 2),
		});
		if (value === undefined) return;
		try {
			const next = structuredClone(this.models);
			const nextModels = next.providers[providerId]?.models;
			if (!nextModels) throw new Error("Provider models are no longer available");
			nextModels[index] = JSON.parse(value) as typeof model;
			ModelConfig.validate(next);
			this.models = next;
		} catch (error) {
			this.options.status(`Invalid model JSON: ${error instanceof Error ? error.message : String(error)}`, "error");
		}
	}

	private async save(): Promise<void> {
		const manager = this.runtime.services.settingsManager;
		if (this.scope === "project" && !manager.isProjectTrusted())
			throw new Error("Project settings require a trusted workspace");
		ModelConfig.validate(this.models);
		const agentDir = this.runtime.services.agentDir;
		const modelRuntime = this.runtime.session.modelRuntime;
		const paths = [
			join(agentDir, "settings.json"),
			join(this.runtime.cwd, CONFIG_DIR_NAME, "settings.json"),
			join(agentDir, "models.json"),
			join(agentDir, "auth.json"),
			join(agentDir, "forge.json"),
			join(agentDir, "forge-auth.json"),
		];
		const journal = SettingsTransactionJournal.begin(agentDir, paths);
		try {
			journal.markApplying();
			await manager.replaceScope("global", this.global);
			if (manager.isProjectTrusted()) await manager.replaceScope("project", this.project);
			ModelConfig.save(join(agentDir, "models.json"), this.models);
			saveForgeConfig(agentDir, this.forge);
			const forgeCredentials = new ForgeCredentialStore(agentDir);
			for (const [key, token] of this.forgeCredentials) {
				if (token) forgeCredentials.set(key, { type: "token", token });
				else forgeCredentials.remove(key);
			}
			for (const [providerId, key] of this.credentials) {
				if (key) await modelRuntime.setProviderCredential(providerId, { type: "api_key", key } as Credential);
				else await modelRuntime.clearProviderCredential(providerId);
			}
			journal.commit();
			await manager.reload();
			await modelRuntime.reloadConfig();
			await this.options.onPresentationChanged?.();
			this.options.status("Settings saved");
		} catch (error) {
			journal.rollback();
			await manager.reload();
			await modelRuntime.reloadConfig();
			this.options.status(
				`Settings were not saved: ${error instanceof Error ? error.message : String(error)}`,
				"error",
			);
		}
	}
}
