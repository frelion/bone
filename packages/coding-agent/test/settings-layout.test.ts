import { stripVTControlCharacters } from "node:util";
import { setKeybindings, visibleWidth } from "@frelion/bone-tui";
import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import { ModelConfig, type ModelsJson } from "../src/core/model-config.ts";
import type { Settings } from "../src/core/settings-manager.ts";
import { ModalShell } from "../src/modes/interactive/components/modal-shell.ts";
import {
	SettingsCenterComponent,
	SettingsCenterSaveError,
} from "../src/modes/interactive/components/settings-center.ts";
import { fitLine, joinColumns } from "../src/modes/interactive/components/terminal-layout.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

beforeAll(() => {
	initTheme("dark");
});

beforeEach(() => {
	setKeybindings(new KeybindingsManager());
});

describe("terminal layout primitives", () => {
	it("pads ANSI-styled and CJK text by terminal columns rather than string length", () => {
		const styled = `${theme.fg("accent", "账户")}: ${theme.bold("ikuncode")}`;
		const fitted = fitLine(styled, 22);
		const columns = joinColumns(styled, 22, " │ ", theme.fg("muted", "已保存"), 12);

		expect(visibleWidth(fitted)).toBe(22);
		expect(visibleWidth(columns)).toBe(37);
		expect(stripVTControlCharacters(columns)).toMatch(/^账户: ikuncode\s+ │ 已保存\s+$/u);
	});
});

describe("ModalShell", () => {
	it("keeps its frame and footer visible when the body is longer than the overlay viewport", () => {
		const shell = new ModalShell({
			title: () => theme.bold("Settings"),
			renderHeader: () => [theme.fg("accent", "Global / Project")],
			renderBody: () => Array.from({ length: 40 }, (_, index) => theme.fg("muted", `Row ${index} · 配置`)),
			renderFooter: () => [theme.fg("muted", "Ctrl+S save · Esc cancel")],
		});
		shell.setViewportRows(12);

		const lines = shell.render(74);
		const plain = lines.map(stripVTControlCharacters);

		expect(lines).toHaveLength(12);
		expect(lines.every((line) => visibleWidth(line) === 74)).toBe(true);
		expect(plain[0]).toMatch(/^┌/u);
		expect(plain.at(-1)).toMatch(/^└/u);
		expect(plain.join("\n")).toContain("Ctrl+S save");
		expect(plain.join("\n")).not.toContain("Row 39");
	});
});

describe("SettingsCenterComponent layout", () => {
	it("configures the current Forge instance and masks a staged token", async () => {
		let saved:
			| Parameters<NonNullable<ConstructorParameters<typeof SettingsCenterComponent>[0]["onSave"]>>[0]
			| undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			forge: {
				repository: {
					provider: "gitlab",
					host: "gitlab.company.test",
					projectPath: "team/service",
					remoteName: "origin",
					remoteUrl: "git@gitlab.company.test:team/service.git",
					rootDir: "/workspace/service",
				},
				config: { instances: [] },
				configuredCredentialKeys: [],
			},
			onSave: async (request) => {
				saved = request;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(24);
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\x1b[c");
		expect(stripVTControlCharacters(settings.render(100).join("\n"))).toContain("gitlab.company.test/team/service");
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\r");
		const token = "glpat-settings-secret";
		settings.handleInput(token);
		expect(stripVTControlCharacters(settings.render(100).join("\n"))).not.toContain(token);
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;

		expect(saved?.forge).toMatchObject({
			config: {
				instances: [
					{
						provider: "gitlab",
						host: "gitlab.company.test",
						apiBaseUrl: "https://gitlab.company.test",
					},
				],
			},
			credential: { key: "gitlab:gitlab.company.test", token },
		});
	});

	it("discards a staged Forge token when the platform identity changes", async () => {
		let saved:
			| Parameters<NonNullable<ConstructorParameters<typeof SettingsCenterComponent>[0]["onSave"]>>[0]
			| undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			forge: {
				repository: {
					provider: "gitlab",
					host: "forge.company.test",
					projectPath: "team/service",
					remoteName: "origin",
					remoteUrl: "git@forge.company.test:team/service.git",
					rootDir: "/workspace/service",
				},
				config: { instances: [] },
				configuredCredentialKeys: [],
			},
			onSave: async (request) => {
				saved = request;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(24);
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\x1b[c");
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\r");
		settings.handleInput("glpat-discarded-draft");
		settings.handleInput("\r");
		for (let index = 0; index < 4; index++) settings.handleInput("\x1b[A");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;

		expect(saved?.forge?.config.instances).toMatchObject([
			{
				provider: "github",
				host: "forge.company.test",
				credential: "github:forge.company.test",
			},
		]);
		expect(saved?.forge?.credential).toBeUndefined();
	});

	it("preserves another provider configured on the same Forge host", async () => {
		let saved:
			| Parameters<NonNullable<ConstructorParameters<typeof SettingsCenterComponent>[0]["onSave"]>>[0]
			| undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			forge: {
				repository: {
					provider: "gitlab",
					host: "forge.company.test",
					projectPath: "team/service",
					remoteName: "origin",
					remoteUrl: "git@forge.company.test:team/service.git",
					rootDir: "/workspace/service",
				},
				config: {
					instances: [
						{
							provider: "gitlab",
							host: "forge.company.test",
							apiBaseUrl: "https://forge.company.test",
							allowPrivateNetwork: false,
						},
						{
							provider: "github",
							host: "forge.company.test",
							apiBaseUrl: "https://forge.company.test/api",
							allowPrivateNetwork: false,
						},
					],
				},
				configuredCredentialKeys: [],
			},
			onSave: async (request) => {
				saved = request;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\x1b[c");
		for (let index = 0; index < 3; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;

		expect(saved?.forge?.config.instances).toEqual([
			{
				provider: "github",
				host: "forge.company.test",
				apiBaseUrl: "https://forge.company.test/api",
				allowPrivateNetwork: false,
			},
			{
				provider: "gitlab",
				host: "forge.company.test",
				apiBaseUrl: "https://forge.company.test",
				allowPrivateNetwork: true,
			},
		]);
	});

	it("does not submit Forge changes when saving project settings without editing Forge", async () => {
		let saved:
			| Parameters<NonNullable<ConstructorParameters<typeof SettingsCenterComponent>[0]["onSave"]>>[0]
			| undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			forge: {
				repository: {
					provider: "gitlab",
					host: "forge.company.test",
					projectPath: "team/service",
					remoteName: "origin",
					remoteUrl: "git@forge.company.test:team/service.git",
					rootDir: "/workspace/service",
				},
				config: { instances: [] },
				configuredCredentialKeys: [],
			},
			onSave: async (request) => {
				saved = request;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.handleInput("\x1b[a");
		settings.handleInput("\r");
		expect(stripVTControlCharacters(settings.render(100).join("\n"))).toContain("Project scope");
		settings.handleInput("\x13");
		await savedPromise;

		expect(saved?.forge).toBeUndefined();
	});

	it("renders bounded, aligned rows at desktop and compact terminal widths", () => {
		const models = {
			providers: {
				ikuncode: {
					name: "Ikun Code",
					api: "openai-responses",
					baseUrl: "https://ikuncode.example/v1",
					models: [
						{
							id: "gpt-5.6-luna",
							name: "GPT-5.6 Luna",
							contextWindow: 272000,
							maxTokens: 128000,
						},
					],
				},
			},
		} as ModelsJson;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models,
			extensionProviders: [
				{
					providerId: "extension-only",
					sourcePath: "/tmp/extensions/extension-only.ts",
					auth: { configured: false },
					modelCount: 0,
					availableModelCount: 0,
					configuration: "extension-only",
					capabilities: { oauth: false, customStream: false, dynamicModels: false },
				},
			],
			onSave: async () => {},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(16);

		for (const width of [88, 56]) {
			const lines = settings.render(width);
			const plain = lines.map(stripVTControlCharacters);
			expect(lines).toHaveLength(16);
			expect(lines.every((line) => visibleWidth(line) === width)).toBe(true);
			expect(plain[0]).toMatch(/^┌/u);
			expect(plain.at(-1)).toMatch(/^└/u);
			expect(plain.join("\n")).toContain("Providers & Models");
			expect(plain.join("\n")).toContain("Ctrl+S");
		}
	});

	it("keeps the Settings frame, command footer, and selected state usable at 80×24 and 56×20", () => {
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: {
				providers: {
					ikuncode: {
						baseUrl: "https://ikuncode.example/v1",
						api: "openai-responses",
						authHeader: false,
						models: [{ id: "gpt-5.6-luna" }],
					},
				},
			} as ModelsJson,
			onSave: async () => {},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.handleInput("\t");
		settings.handleInput("\x1b[B");
		for (const [width, rows] of [
			[80, 24],
			[56, 20],
		] as const) {
			settings.setViewportRows(rows);
			const lines = settings.render(width);
			const plain = lines.map(stripVTControlCharacters).join("\n");
			expect(lines).toHaveLength(rows);
			expect(lines.every((line) => visibleWidth(line) === width)).toBe(true);
			expect(plain).toContain("Settings");
			expect(plain).toContain("Ctrl+S");
			expect(plain).toContain("› + Add provider");
		}
	});

	it("moves between the visible scope, pages, content, and footer regions with Shift+arrows", () => {
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			onSave: async () => {},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(20);

		settings.handleInput("\x1b[a"); // Shift+Up: Pages -> Scope
		let rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(rendered).toContain("› Global / Project");

		settings.handleInput("\x1b[c"); // Shift+Right: Global -> Project
		rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(rendered).toContain("Project scope");

		settings.handleInput("\x1b[b"); // Shift+Down: Scope -> Pages
		settings.handleInput("\x1b[c"); // Shift+Right: Pages -> Content
		rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(rendered).toContain("Focus · Providers & Models");

		settings.handleInput("\x1b[b"); // Shift+Down: Content -> Footer
		rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(rendered).toContain("› Save");

		settings.handleInput("\x1b[d"); // Shift+Left: Save -> Cancel
		rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(rendered).toContain("› Cancel");
	});

	it("uses one inline Provider form to add a model without asking for the Provider again", async () => {
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: {
				providers: {
					ikuncode: {
						name: "Ikun Code",
						baseUrl: "https://ikuncode.example/v1",
						api: "openai-responses",
						models: [{ id: "gpt-5.6-luna", name: "GPT-5.6 Luna" }],
					},
				},
			} as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(32);

		settings.handleInput("\t");
		settings.handleInput("\r"); // Provider · ikuncode
		let rendered = stripVTControlCharacters(settings.render(120).join("\n"));
		expect(rendered).toContain("Edit provider");
		expect(rendered).toContain("Basic configuration");
		expect(rendered).toContain("Models  1 configured");
		expect(rendered).not.toContain("Existing Provider ID");

		for (let index = 0; index < 10; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r"); // Add model manually.
		rendered = stripVTControlCharacters(settings.render(120).join("\n"));
		expect(rendered).toContain("Add model manually");
		expect(rendered).toContain("> ");
		settings.handleInput("gpt-5.6-sol");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await new Promise((resolve) => setTimeout(resolve, 0));

		expect(saved?.providers.ikuncode?.models).toContainEqual({ id: "gpt-5.6-sol" });
	});

	it("stays open and confirms success after Ctrl+S", async () => {
		let saved = 0;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			onSave: async () => {
				saved++;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(20);
		settings.handleInput("\x13");
		await new Promise((resolve) => setTimeout(resolve, 0));

		const rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(saved).toBe(1);
		expect(rendered).toContain("Settings");
		expect(rendered).toContain("Saved — changes are applied. Esc closes Settings.");
	});

	it("adds a model from its Provider context and saves it under that Provider", async () => {
		let saved: ModelsJson | undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: {
				providers: {
					ikuncode: {
						baseUrl: "https://ikuncode.example/v1",
						api: "openai-responses",
						authHeader: false,
						models: [{ id: "gpt-5.6-luna" }],
					},
				},
			} as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
			onDiscoverModels: async () => [{ id: "gpt-5.6-sol", name: "GPT-5.6 Sol" }],
		});

		settings.handleInput("\t");
		settings.handleInput("\r");
		for (let index = 0; index < 11; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		await new Promise((resolve) => setTimeout(resolve, 0));
		const rendered = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(rendered).toContain("Filter fetched models");
		settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;

		expect(saved?.providers.ikuncode?.models).toContainEqual({
			id: "gpt-5.6-sol",
			name: "GPT-5.6 Sol",
		});
	});

	it("closes inline editing before returning from the Provider form and closing Settings", () => {
		let cancelled = 0;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [{ id: "gpt-5.6-luna" }] } } } as ModelsJson,
			onSave: async () => {},
			onCancel: () => {
				cancelled++;
			},
			onStartOAuth: () => {},
		});

		settings.handleInput("\t");
		settings.handleInput("\r");
		for (let index = 0; index < 10; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		expect(stripVTControlCharacters(settings.render(120).join("\n"))).toContain("Add model manually");

		settings.handleInput("\x1b");
		expect(stripVTControlCharacters(settings.render(120).join("\n"))).toContain("Edit provider");
		expect(cancelled).toBe(0);

		settings.handleInput("\x1b");
		expect(stripVTControlCharacters(settings.render(120).join("\n"))).toContain("Providers");
		expect(cancelled).toBe(0);

		settings.handleInput("\x1b");
		expect(cancelled).toBe(1);
	});

	it("stages one Provider API key without rendering it and saves it with the Provider", async () => {
		let saved:
			| Parameters<NonNullable<ConstructorParameters<typeof SettingsCenterComponent>[0]["onSave"]>>[0]
			| undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const key = "provider-key-never-rendered";
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [] } } } as ModelsJson,
			providerAuthentication: { ikuncode: { type: "api_key", oauthAvailable: true } },
			onSave: async (request) => {
				saved = request;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});

		settings.handleInput("\t");
		settings.handleInput("\r");
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput(key);
		settings.handleInput("\r");

		const rendered = stripVTControlCharacters(settings.render(120).join("\n"));
		expect(rendered).toContain("API key  Staged");
		expect(rendered).not.toContain(key);

		settings.handleInput("\x13");
		await savedPromise;
		expect(saved?.credentials).toEqual([{ providerId: "ikuncode", credential: { type: "api_key", key } }]);
	});

	it("stages explicit Provider authentication removal until saved", async () => {
		let saved:
			| Parameters<NonNullable<ConstructorParameters<typeof SettingsCenterComponent>[0]["onSave"]>>[0]
			| undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [] } } } as ModelsJson,
			providerAuthentication: { ikuncode: { type: "oauth", oauthAvailable: true } },
			onSave: async (request) => {
				saved = request;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});

		settings.handleInput("\t");
		settings.handleInput("\r");
		for (let index = 0; index < 7; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;
		expect(saved?.credentials).toEqual([{ providerId: "ikuncode", credential: undefined }]);
	});

	it("starts OAuth with the selected Provider ID", () => {
		let providerId: string | undefined;
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [] } } } as ModelsJson,
			providerAuthentication: { ikuncode: { oauthAvailable: true } },
			onSave: async () => {},
			onCancel: () => {},
			onStartOAuth: (id) => {
				providerId = id;
			},
		});

		settings.handleInput("\t");
		settings.handleInput("\r");
		for (let index = 0; index < 6; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		expect(providerId).toBe("ikuncode");
	});

	it("does not save staged Provider authentication when Settings is cancelled with Escape", () => {
		let saves = 0;
		let cancellations = 0;
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [] } } } as ModelsJson,
			onSave: async () => {
				saves++;
			},
			onCancel: () => {
				cancellations++;
			},
			onStartOAuth: () => {},
		});
		settings.handleInput("\t");
		settings.handleInput("\r");
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("staged-key");
		settings.handleInput("\r");
		settings.handleInput("\x1b");
		settings.handleInput("\x1b");
		expect(saves).toBe(0);
		expect(cancellations).toBe(1);
	});

	it("shows complete extension provider runtime status without exposing function configuration", () => {
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			extensionProviders: [
				{
					providerId: "dynamic-provider",
					sourcePath: "/tmp/bone/extensions/dynamic-provider.ts",
					auth: { configured: true, source: "runtime", label: "session credential" },
					modelCount: 3,
					availableModelCount: 2,
					compositionError: "model API mismatch",
					configuration: "builtin+models-json+extension",
					capabilities: { oauth: true, customStream: true, dynamicModels: true },
				},
			],
			onSave: async () => {},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.setViewportRows(24);
		const rendered = stripVTControlCharacters(settings.render(180).join("\n"));
		expect(rendered).toContain("Extension runtime providers");
		expect(rendered).toContain("dynamic-provider · builtin + models-json + extension");
		expect(rendered).toContain("source /tmp/bone/extensions/dynamic-provider.ts");
		expect(rendered).toContain("auth configured · runtime (session credential)");
		expect(rendered).toContain("models 2/3 available · OAuth, custom stream, dynamic models");
		expect(rendered).toContain("composition error: model API mismatch");
	});

	it("edits numeric settings in the modal draft before saving", async () => {
		let saved: Settings | undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: { compaction: { reserveTokens: 16384 } },
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			onSave: async (request) => {
				saved = request.global;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});

		settings.handleInput("\x1b[B");
		settings.handleInput("\x1b[B");
		settings.handleInput("\t");
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\x01");
		for (let index = 0; index < 5; index++) settings.handleInput("\x1b[3~");
		settings.handleInput("12000");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;

		expect(saved?.compaction?.reserveTokens).toBe(12000);
	});

	it("stages a theme selection until the settings modal is saved", async () => {
		let saved: Settings | undefined;
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			onSave: async (request) => {
				saved = request.global;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		for (let index = 0; index < 3; index++) settings.handleInput("\x1b[B");
		settings.handleInput("\t");
		settings.handleInput("\r");
		expect(saved).toBeUndefined();
		settings.handleInput("\x13");
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(saved?.theme).toBe("dark");
	});

	it("uses a visual, confirmed picker before deleting a model definition", async () => {
		let saved: ModelsJson | undefined;
		let resolveSaved: (() => void) | undefined;
		const savedPromise = new Promise<void>((resolve) => {
			resolveSaved = resolve;
		});
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: { ikun: { models: [{ id: "luna" }] } } } as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
				resolveSaved?.();
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.handleInput("\t");
		settings.handleInput("\x1bd");
		expect(stripVTControlCharacters(settings.render(100).join("\n"))).toContain("Delete model configuration");
		settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await savedPromise;
		expect(saved?.providers.ikun?.models).toEqual([]);
	});

	it("lists provider, model, and override targets in the visual deletion picker", async () => {
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: {
				providers: {
					ikun: {
						models: [{ id: "luna" }],
						modelOverrides: { "luna-*": { name: "Luna family" } },
					},
				},
			} as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.handleInput("\t");
		settings.handleInput("\x1bd");
		const picker = stripVTControlCharacters(settings.render(100).join("\n"));
		expect(picker).toContain("Provider · ikun");
		expect(picker).toContain("Model · ikun / luna");
		expect(picker).toContain("Override · ikun / luna-*");
		settings.handleInput("\x1b[B");
		settings.handleInput("\x1b[B");
		settings.handleInput("\r");
		settings.handleInput("\r");
		settings.handleInput("\x13");
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(saved?.providers.ikun?.models).toEqual([{ id: "luna" }]);
		expect(saved?.providers.ikun?.modelOverrides).toEqual({});
	});

	it("returns to the affected settings page when save validation identifies it", async () => {
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: {} } as ModelsJson,
			onSave: async () => {
				throw new SettingsCenterSaveError("providers", "Invalid provider field");
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.handleInput("\x1b[B");
		settings.handleInput("\t");
		settings.handleInput("\x13");
		await new Promise((resolve) => setTimeout(resolve, 0));
		const rendered = stripVTControlCharacters(settings.render(140).join("\n"));
		expect(rendered).toContain("› Providers & Models");
		expect(rendered).toContain("Invalid provider field");
	});

	it("marks and scrolls to the exact model field reported by save validation", async () => {
		const settings = new SettingsCenterComponent({
			global: {},
			project: {},
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [{ id: "broken-model" }] } } } as ModelsJson,
			onSave: async () => {
				throw new SettingsCenterSaveError("providers", "Invalid max tokens", undefined, {
					providerId: "ikuncode",
					modelId: "broken-model",
					field: "maxTokens",
				});
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		settings.handleInput("\x1b[B");
		settings.handleInput("\t");
		settings.handleInput("\x13");
		await new Promise((resolve) => setTimeout(resolve, 0));
		const rendered = stripVTControlCharacters(settings.render(140).join("\n"));
		expect(rendered).toContain("Model · broken-model");
		expect(rendered).toContain("Invalid max tokens");
	});

	it("edits advanced model fields transactionally and discards them on editor escape", async () => {
		const initialModels = {
			providers: {
				ikuncode: {
					name: "Ikun Code",
					models: [{ id: "gpt-5.6-luna", name: "GPT-5.6 Luna" }],
				},
			},
		} as ModelsJson;
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: initialModels,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		const submit = (value: string) => {
			settings.handleInput(value);
			settings.handleInput("\r");
		};

		settings.handleInput("\t");
		settings.handleInput("\x1be");
		submit("ikuncode");
		submit("gpt-5.6-luna");
		submit("GPT-5.6 Luna");
		submit("");
		submit("https://models.example/v1");
		submit("on");
		submit("text,image");
		submit("");
		submit("");
		settings.handleInput("\x13");
		await Promise.resolve();

		expect(saved?.providers.ikuncode?.models?.[0]).toMatchObject({
			baseUrl: "https://models.example/v1",
			reasoning: true,
			input: ["text", "image"],
		});

		settings.handleInput("\x1be");
		submit("ikuncode");
		submit("gpt-5.6-luna");
		submit("Changed but cancelled");
		settings.handleInput("\x1b");
		settings.handleInput("\x13");
		await Promise.resolve();

		expect(saved?.providers.ikuncode?.models?.[0]?.name).toBe("GPT-5.6 Luna");
	});

	it("edits thinking levels, base costs, and cost tiers without a JSON editor", async () => {
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [{ id: "gpt-5.6-luna" }] } } } as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		const submit = (value: string) => {
			settings.handleInput(value);
			settings.handleInput("\r");
		};

		settings.handleInput("\t");
		settings.handleInput("\x1ba");
		submit("ikuncode");
		submit("gpt-5.6-luna");
		submit("none");
		for (let index = 0; index < 6; index++) submit("");
		submit("1");
		submit("2");
		submit("0.5");
		submit("1.5");
		submit("add");
		submit("1000000");
		submit("3");
		submit("4");
		submit("1.5");
		submit("3.5");
		submit("done");
		settings.handleInput("\x13");
		await Promise.resolve();

		expect(saved?.providers.ikuncode?.models?.[0]).toMatchObject({
			thinkingLevelMap: { off: "none" },
			cost: {
				input: 1,
				output: 2,
				cacheRead: 0.5,
				cacheWrite: 1.5,
				tiers: [
					{
						inputTokensAbove: 1000000,
						input: 3,
						output: 4,
						cacheRead: 1.5,
						cacheWrite: 3.5,
					},
				],
			},
		});
	});

	it("edits typed provider compatibility fields and chat template variables", async () => {
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: { ikuncode: { api: "openai-completions", models: [] } } } as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		const submit = (value: string) => {
			settings.handleInput(value);
			settings.handleInput("\r");
		};

		settings.handleInput("\t");
		settings.handleInput("\x1bc");
		submit("ikuncode");
		submit("");
		submit("supportsDeveloperRole");
		submit("on");
		submit("openRouterRouting.order");
		submit("provider-a, provider-b");
		submit("chatTemplateKwargs");
		submit("reasoning_effort");
		submit("variable:thinking.effort:omitWhenOff");
		submit("done");
		settings.handleInput("\x13");
		await Promise.resolve();

		expect(saved?.providers.ikuncode?.compat).toMatchObject({
			supportsDeveloperRole: true,
			openRouterRouting: { order: ["provider-a", "provider-b"] },
			chatTemplateKwargs: { reasoning_effort: { $var: "thinking.effort", omitWhenOff: true } },
		});
		expect(() => ModelConfig.validate(saved!)).not.toThrow();
	});

	it("applies behavior, costs, and compatibility settings to a model override target", async () => {
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: { ikuncode: { api: "openai-completions", models: [] } } } as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		const submit = (value: string) => {
			settings.handleInput(value);
			settings.handleInput("\r");
		};

		settings.handleInput("\t");
		settings.handleInput("\x1ba");
		submit("ikuncode");
		submit("override:gpt-5.6-*");
		submit("none");
		for (let index = 0; index < 6; index++) submit("");
		submit("1");
		submit("2");
		submit("0.5");
		submit("1.5");
		submit("done");

		settings.handleInput("\x1bc");
		submit("ikuncode");
		submit("override:gpt-5.6-*");
		submit("supportsDeveloperRole");
		submit("on");
		submit("done");
		settings.handleInput("\x13");
		await Promise.resolve();

		expect(saved?.providers.ikuncode?.modelOverrides?.["gpt-5.6-*"]).toMatchObject({
			thinkingLevelMap: { off: "none" },
			cost: { input: 1, output: 2, cacheRead: 0.5, cacheWrite: 1.5 },
			compat: { supportsDeveloperRole: true },
		});
		expect(() => ModelConfig.validate(saved!)).not.toThrow();
	});

	it("writes headers to an override target", async () => {
		let saved: ModelsJson | undefined;
		const settings = new SettingsCenterComponent({
			global: {} as Settings,
			project: {} as Settings,
			projectTrusted: true,
			models: { providers: { ikuncode: { models: [] } } } as ModelsJson,
			onSave: async (request) => {
				saved = request.models;
			},
			onCancel: () => {},
			onStartOAuth: () => {},
		});
		const submit = (value: string) => {
			settings.handleInput(value);
			settings.handleInput("\r");
		};

		settings.handleInput("\t");
		settings.handleInput("\x1bh");
		submit("ikuncode");
		submit("override:gpt-5.6-*");
		submit("X-Workspace");
		submit("bone");
		settings.handleInput("\x13");
		await Promise.resolve();

		expect(saved?.providers.ikuncode?.modelOverrides?.["gpt-5.6-*"]?.headers).toEqual({ "X-Workspace": "bone" });
		expect(() => ModelConfig.validate(saved!)).not.toThrow();
	});
});
