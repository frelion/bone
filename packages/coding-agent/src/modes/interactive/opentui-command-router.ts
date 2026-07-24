import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Api, AuthEvent, AuthPrompt, Model } from "@frelion/bone-ai";
import { type AutocompleteProvider, CombinedAutocompleteProvider } from "@frelion/bone-tui";
import { getShareViewerUrl } from "../../config.ts";
import type { AgentSession } from "../../core/agent-session.ts";
import type { AgentSessionRuntime } from "../../core/agent-session-runtime.ts";
import type { ExtensionUIV2Context } from "../../core/extensions/ui-v2.ts";
import { BUILTIN_SLASH_COMMANDS, type BuiltinSlashCommand, type SlashCommandInfo } from "../../core/slash-commands.ts";
import { getProjectTrustOptions, ProjectTrustStore } from "../../core/trust-manager.ts";
import { copyToClipboard } from "../../utils/clipboard.ts";
import { OpenTUISettingsCenter } from "./components/opentui-settings-center.ts";

export interface OpenTUICommandHost {
	readonly current: AgentSessionRuntime;
	createNew(): Promise<void>;
}

export interface OpenTUICommandRouterOptions {
	host: OpenTUICommandHost;
	getUI: () => ExtensionUIV2Context | undefined;
	onStatus: (message: string, tone?: "info" | "warning" | "error") => void;
	onFocusConversations: () => void;
	onQuit: () => void;
	onReloaded?: () => void | Promise<void>;
	onShare?: (runtime: AgentSessionRuntime) => Promise<string>;
	onShowChangelog?: () => void | Promise<void>;
	onShowHotkeys?: () => void | Promise<void>;
	onPresentationChanged?: () => void | Promise<void>;
}

export type OpenTUICommandResult = { handled: false } | { handled: true; kind: "command" | "bash" };

function parseCommand(text: string): { name: string; argument: string } | undefined {
	if (!text.startsWith("/")) return undefined;
	const separator = text.search(/\s/);
	return separator === -1
		? { name: text.slice(1).toLowerCase(), argument: "" }
		: { name: text.slice(1, separator).toLowerCase(), argument: text.slice(separator).trim() };
}

function resolveBuiltinCommand(name: string): string | undefined {
	const exact = BUILTIN_SLASH_COMMANDS.find((item) => item.name === name);
	if (exact) return exact.name;
	const prefixMatches = BUILTIN_SLASH_COMMANDS.filter((item) => item.name.startsWith(name));
	return prefixMatches.length === 1 ? prefixMatches[0]?.name : undefined;
}

function requireUI(ui: ExtensionUIV2Context | undefined): ExtensionUIV2Context {
	if (!ui?.available) throw new Error("This command requires the interactive OpenTUI dialog host");
	return ui;
}

function modelKey(model: Model<Api>): string {
	return `${model.provider}/${model.id}`;
}

function entryLabel(entry: ReturnType<AgentSession["sessionManager"]["getBranch"]>[number]): string {
	if (entry.type !== "message") return `${entry.type} · ${entry.id.slice(0, 8)}`;
	const content = "content" in entry.message ? entry.message.content : "";
	const text =
		typeof content === "string"
			? content
			: Array.isArray(content)
				? content.map((part) => (part.type === "text" ? part.text : "")).join(" ")
				: "";
	return `${entry.message.role} · ${text.replace(/\s+/g, " ").trim().slice(0, 80) || entry.id.slice(0, 8)}`;
}

/** Fixed built-in command boundary for the OpenTUI composer. */
export class OpenTUICommandRouter {
	private readonly options: OpenTUICommandRouterOptions;

	constructor(options: OpenTUICommandRouterOptions) {
		this.options = options;
	}

	createAutocompleteProvider(cwd = this.options.host.current.cwd): AutocompleteProvider {
		const commands = new Map<string, BuiltinSlashCommand | SlashCommandInfo>(
			BUILTIN_SLASH_COMMANDS.map((command) => [command.name, command]),
		);
		for (const command of this.options.host.current.session.getSlashCommands?.() ?? []) {
			if (!commands.has(command.name)) commands.set(command.name, command);
		}
		return new CombinedAutocompleteProvider([...commands.values()], cwd);
	}

	async route(text: string, runtime = this.options.host.current): Promise<OpenTUICommandResult> {
		const trimmed = text.trim();
		if (trimmed.startsWith("!")) {
			await this.executeBash(trimmed, runtime);
			return { handled: true, kind: "bash" };
		}
		const command = parseCommand(trimmed);
		if (!command) return { handled: false };
		const resolvedName = resolveBuiltinCommand(command.name);
		if (!resolvedName) return { handled: false };
		await this.executeCommand(resolvedName, command.argument, runtime);
		return { handled: true, kind: "command" };
	}

	private async executeCommand(name: string, argument: string, runtime: AgentSessionRuntime): Promise<void> {
		const session = runtime.session;
		switch (name) {
			case "settings":
				await this.settings(runtime);
				return;
			case "model":
				await this.model(runtime, argument);
				return;
			case "scoped-models":
				await this.scopedModels(runtime);
				return;
			case "trust":
				await this.trust(runtime);
				return;
			case "login":
				await this.login(runtime, argument);
				return;
			case "logout":
				await this.logout(runtime);
				return;
			case "new":
				await this.options.host.createNew();
				this.status("Started a new conversation");
				return;
			case "compact":
				await session.compact(argument || undefined);
				this.status("Conversation compacted");
				return;
			case "plan":
				if (session.collaborationMode === "plan") session.exitPlanMode();
				else session.enterPlanMode();
				this.status(`Plan mode ${session.collaborationMode === "plan" ? "enabled" : "disabled"}`);
				return;
			case "reload":
				await session.reload();
				await this.options.onReloaded?.();
				this.status("Reloaded local resources");
				return;
			case "name":
				await this.name(session, argument);
				return;
			case "conversation":
				this.conversation(session);
				return;
			case "conversations":
				this.options.onFocusConversations();
				return;
			case "quit":
				this.options.onQuit();
				return;
			case "export":
				await this.export(session, argument);
				return;
			case "import":
				await this.import(runtime, argument);
				return;
			case "copy":
				await this.copy(session);
				return;
			case "fork":
				await this.fork(runtime);
				return;
			case "clone":
				await this.clone(runtime);
				return;
			case "tree":
				await this.tree(session);
				return;
			case "status":
				this.runtimeStatus(runtime, argument);
				return;
			case "share":
				await this.share(runtime);
				return;
			case "changelog":
				if (this.options.onShowChangelog) await this.options.onShowChangelog();
				else this.status("Changelog is available in packages/coding-agent/CHANGELOG.md");
				return;
			case "hotkeys":
				if (this.options.onShowHotkeys) await this.options.onShowHotkeys();
				else this.status("OpenTUI uses fixed keys: Enter submit, Shift+Enter newline, Ctrl+C abort, Ctrl+D quit");
				return;
			default:
				throw new Error(`Unsupported built-in command: /${name}`);
		}
	}

	private async settings(runtime: AgentSessionRuntime): Promise<void> {
		const ui = requireUI(this.options.getUI());
		await new OpenTUISettingsCenter({
			runtime,
			ui,
			onLogin: (providerId) => this.login(runtime, providerId),
			onLogout: () => this.logout(runtime),
			onPresentationChanged: this.options.onPresentationChanged,
			status: this.options.onStatus,
		}).run();
	}

	private async model(runtime: AgentSessionRuntime, query: string): Promise<void> {
		const session = runtime.session;
		const available = await session.modelRuntime.getAvailable();
		const normalized = query.toLowerCase();
		const matches = normalized
			? available.filter(
					(model) =>
						modelKey(model).toLowerCase().includes(normalized) || model.name?.toLowerCase().includes(normalized),
				)
			: available;
		if (matches.length === 0) throw new Error(query ? `No model matches ${query}` : "No models are available");
		let selected = matches.length === 1 ? modelKey(matches[0]!) : undefined;
		if (!selected) {
			selected = await requireUI(this.options.getUI()).dialogs.select({
				title: "Select model",
				options: matches.map((model) => ({
					value: modelKey(model),
					label: model.name || model.id,
					description: modelKey(model),
				})),
				initialValue: session.model ? modelKey(session.model) : undefined,
			});
		}
		if (!selected) return;
		const model = matches.find((candidate) => modelKey(candidate) === selected);
		if (!model) throw new Error(`Model is no longer available: ${selected}`);
		await session.setModel(model);
		this.status(`Model: ${modelKey(model)}`);
	}

	private async scopedModels(runtime: AgentSessionRuntime): Promise<void> {
		const session = runtime.session;
		const models = await session.modelRuntime.getAvailable();
		if (models.length === 0) throw new Error("No models are available");
		const selected = new Set(session.scopedModels.map((item) => modelKey(item.model)));
		const ui = requireUI(this.options.getUI());
		for (;;) {
			const value = await ui.dialogs.select({
				title: "Model cycling scope",
				options: [
					{ value: "__done__", label: "Done", description: `${selected.size} models enabled` },
					...models.map((model) => {
						const key = modelKey(model);
						return {
							value: key,
							label: `${selected.has(key) ? "[x]" : "[ ]"} ${model.name || model.id}`,
							description: key,
						};
					}),
				],
			});
			if (!value || value === "__done__") break;
			if (selected.has(value)) selected.delete(value);
			else selected.add(value);
		}
		session.setScopedModels(models.filter((model) => selected.has(modelKey(model))).map((model) => ({ model })));
		this.status(selected.size === 0 ? "Model cycling scope cleared" : `${selected.size} scoped models enabled`);
	}

	private async trust(runtime: AgentSessionRuntime): Promise<void> {
		const choices = getProjectTrustOptions(runtime.cwd);
		const selected = await requireUI(this.options.getUI()).dialogs.select({
			title: "Workspace trust",
			options: choices.map((choice, index) => ({ value: String(index), label: choice.label })),
		});
		if (selected === undefined) return;
		const choice = choices[Number(selected)];
		if (!choice) return;
		new ProjectTrustStore(runtime.services.agentDir).setMany(choice.updates);
		this.status(choice.savedPath ? `${choice.label}: ${choice.savedPath}` : choice.label);
	}

	private async login(runtime: AgentSessionRuntime, providerRef: string): Promise<void> {
		const modelRuntime = runtime.session.modelRuntime;
		const providers = modelRuntime.getProviders();
		let providerId = providerRef;
		if (!providerId) {
			providerId =
				(await requireUI(this.options.getUI()).dialogs.select({
					title: "Login provider",
					options: providers.map((provider) => ({ value: provider.id, label: provider.name })),
				})) ?? "";
		}
		const provider = providers.find((candidate) => candidate.id === providerId);
		if (!provider) throw new Error(providerId ? `Unknown provider: ${providerId}` : "Login cancelled");
		const methods = [provider.auth.oauth && "oauth", provider.auth.apiKey && "api_key"].filter(
			(method): method is "oauth" | "api_key" => Boolean(method),
		);
		const method =
			methods.length === 1
				? methods[0]
				: await requireUI(this.options.getUI()).dialogs.select({
						title: `Login to ${provider.name}`,
						options: methods.map((value) => ({ value, label: value === "oauth" ? "OAuth" : "API key" })),
					});
		if (!method) return;
		const ui = requireUI(this.options.getUI());
		await modelRuntime.login(provider.id, method, {
			prompt: async (prompt: AuthPrompt) => {
				if (prompt.type === "select") {
					return (
						(await ui.dialogs.select({
							title: prompt.message,
							options: prompt.options.map((option) => ({
								value: option.id,
								label: option.label,
								description: option.description,
							})),
						})) ?? ""
					);
				}
				return (await ui.dialogs.input({ title: prompt.message, placeholder: prompt.placeholder })) ?? "";
			},
			notify: (event: AuthEvent) => this.notifyAuthEvent(ui, event),
		});
		this.status(`Logged in to ${provider.name}`);
	}

	private notifyAuthEvent(ui: ExtensionUIV2Context, event: AuthEvent): void {
		if (event.type === "auth_url")
			ui.dialogs.notify(`${event.url}${event.instructions ? `\n${event.instructions}` : ""}`);
		else if (event.type === "device_code") ui.dialogs.notify(`${event.verificationUri}\nCode: ${event.userCode}`);
		else ui.dialogs.notify(event.message);
	}

	private async logout(runtime: AgentSessionRuntime): Promise<void> {
		const providers = runtime.session.modelRuntime
			.getProviders()
			.filter((provider) => runtime.session.modelRuntime.getProviderAuthStatus(provider.id).configured);
		if (providers.length === 0) throw new Error("No configured providers");
		const selected = await requireUI(this.options.getUI()).dialogs.select({
			title: "Logout provider",
			options: providers.map((provider) => ({ value: provider.id, label: provider.name })),
		});
		if (!selected) return;
		await runtime.session.modelRuntime.logout(selected);
		this.status(`Logged out of ${selected}`);
	}

	private async name(session: AgentSession, argument: string): Promise<void> {
		const name =
			argument ||
			(await requireUI(this.options.getUI()).dialogs.input({
				title: "Conversation name",
				initialValue: session.sessionManager.getSessionName(),
			}));
		if (!name?.trim()) return;
		session.setSessionName(name.trim());
		this.status(`Conversation name: ${name.trim()}`);
	}

	private conversation(session: AgentSession): void {
		const manager = session.sessionManager;
		this.status(
			[
				`Conversation: ${manager.getSessionName() || manager.getSessionId()}`,
				`File: ${manager.getSessionFile() || "in memory"}`,
				`Entries: ${manager.getBranch().length}`,
				`Cwd: ${manager.getCwd()}`,
			].join("\n"),
		);
	}

	private async export(session: AgentSession, argument: string): Promise<void> {
		const path = argument || undefined;
		const output = path?.endsWith(".jsonl") ? session.exportToJsonl(path) : await session.exportToHtml(path);
		this.status(`Conversation exported to: ${output}`);
	}

	private async import(runtime: AgentSessionRuntime, argument: string): Promise<void> {
		const path =
			argument ||
			(await requireUI(this.options.getUI()).dialogs.input({
				title: "Import JSONL",
				placeholder: "path/to/session.jsonl",
			}));
		if (!path) return;
		const result = await runtime.importFromJsonl(path);
		this.status(result.cancelled ? "Import cancelled" : `Conversation imported from: ${path}`);
	}

	private async copy(session: AgentSession): Promise<void> {
		const text = session.getLastAssistantText();
		if (!text) throw new Error("No assistant message to copy");
		await copyToClipboard(text);
		this.status(`Copied ${text.length} characters`);
	}

	private async fork(runtime: AgentSessionRuntime): Promise<void> {
		const entries = runtime.session.sessionManager
			.getBranch()
			.filter((entry) => entry.type === "message" && entry.message.role === "user");
		if (entries.length === 0) throw new Error("No user messages to fork from");
		const id = await requireUI(this.options.getUI()).dialogs.select({
			title: "Fork from message",
			options: entries.map((entry) => ({ value: entry.id, label: entryLabel(entry) })),
		});
		if (!id) return;
		await runtime.fork(id);
		this.status("Forked to a new conversation");
	}

	private async clone(runtime: AgentSessionRuntime): Promise<void> {
		const leaf = runtime.session.sessionManager.getLeafId();
		if (!leaf) throw new Error("Nothing to clone yet");
		await runtime.fork(leaf, { position: "at" });
		this.status("Cloned to a new conversation");
	}

	private async tree(session: AgentSession): Promise<void> {
		const entries = session.sessionManager.getBranch();
		if (entries.length === 0) throw new Error("Conversation tree is empty");
		const id = await requireUI(this.options.getUI()).dialogs.select({
			title: "Conversation tree",
			options: entries.map((entry) => ({ value: entry.id, label: entryLabel(entry) })),
			initialValue: session.sessionManager.getLeafId() ?? undefined,
		});
		if (!id) return;
		await session.navigateTree(id);
		this.status("Navigated conversation tree");
	}

	private runtimeStatus(runtime: AgentSessionRuntime, argument: string): void {
		if (argument) throw new Error("Usage: /status");
		const session = runtime.session;
		this.status(
			[
				`State: ${session.isStreaming ? "streaming" : session.isCompacting ? "compacting" : "idle"}`,
				`Model: ${session.model ? modelKey(session.model) : "none"}`,
				`Mode: ${session.collaborationMode}`,
				`Cwd: ${runtime.cwd}`,
			].join("\n"),
		);
	}

	private async share(runtime: AgentSessionRuntime): Promise<void> {
		if (this.options.onShare) {
			this.status(`Share URL: ${await this.options.onShare(runtime)}`);
			return;
		}
		const directory = await mkdtemp(join(tmpdir(), "bone-share-"));
		const exportPath = join(directory, "conversation.html");
		try {
			await runtime.session.exportToHtml(exportPath);
			const gistUrl = await this.runGitHubCli(["gist", "create", "--public=false", exportPath]);
			const gistId = gistUrl.split("/").filter(Boolean).at(-1);
			if (!gistId) throw new Error(`Could not parse gist URL: ${gistUrl}`);
			const previewUrl = getShareViewerUrl(gistId);
			this.status(previewUrl ? `Share URL: ${previewUrl}\nGist: ${gistUrl}` : `Gist: ${gistUrl}`);
		} finally {
			await rm(directory, { recursive: true, force: true });
		}
	}

	private async runGitHubCli(args: readonly string[]): Promise<string> {
		return await new Promise((resolve, reject) => {
			const child = spawn("gh", [...args], { stdio: ["ignore", "pipe", "pipe"] });
			let stdout = "";
			let stderr = "";
			child.stdout.on("data", (chunk: Buffer) => {
				stdout += chunk.toString();
			});
			child.stderr.on("data", (chunk: Buffer) => {
				stderr += chunk.toString();
			});
			child.on("error", (error) => reject(new Error(`Could not run GitHub CLI: ${error.message}`)));
			child.on("close", (code) => {
				if (code === 0) resolve(stdout.trim());
				else reject(new Error(stderr.trim() || `GitHub CLI exited with code ${code ?? "unknown"}`));
			});
		});
	}

	private async executeBash(text: string, runtime: AgentSessionRuntime): Promise<void> {
		const excluded = text.startsWith("!!");
		const command = text.slice(excluded ? 2 : 1).trim();
		if (!command) {
			this.status("Usage: !<command> or !!<command>", "warning");
			return;
		}
		const session = runtime.session;
		if (session.isBashRunning) throw new Error("A bash command is already running");
		const result = await session.executeBash(command, undefined, { excludeFromContext: excluded });
		this.status(`Bash exited ${result.exitCode}${result.cancelled ? " (cancelled)" : ""}`);
	}

	private status(message: string, tone: "info" | "warning" | "error" = "info"): void {
		this.options.onStatus(message, tone);
	}
}
