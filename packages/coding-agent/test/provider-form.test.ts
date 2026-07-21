import { setKeybindings } from "@frelion/bone-tui";
import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import type { ProviderPreset } from "../src/core/provider-presets.ts";
import { ProviderFormComponent, type ProviderFormDraft } from "../src/modes/interactive/components/provider-form.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";
import { stripAnsi } from "../src/utils/ansi.ts";

const preset = {
	id: "openai",
	label: "OpenAI",
	baseUrl: "https://api.openai.com/v1",
	api: "openai-responses",
} satisfies ProviderPreset;

function render(component: ProviderFormComponent): string {
	return stripAnsi(component.render(120).join("\n"));
}

function createForm(
	draft: ProviderFormDraft = { id: "local", provider: { name: "Local", api: "openai-completions", models: [] } },
	onFetchModels?: (draft: ProviderFormDraft, stagedApiKey: string | undefined) => Promise<readonly { id: string }[]>,
) {
	const draftChanges: ProviderFormDraft[] = [];
	const stagedKeys: Array<{ providerId: string; apiKey: string }> = [];
	const oauthRequests: string[] = [];
	const clears: string[] = [];
	const form = new ProviderFormComponent({
		mode: "edit",
		draft,
		presets: [preset],
		authentication: { oauthAvailable: true },
		callbacks: {
			onDraftChange: (next) => {
				draftChanges.push(next);
			},
			onStageApiKey: (providerId, apiKey) => {
				stagedKeys.push({ providerId, apiKey });
			},
			onStageOAuth: (providerId) => {
				oauthRequests.push(providerId);
			},
			onClearAuthentication: (providerId) => {
				clears.push(providerId);
			},
			onFetchModels,
		},
	});
	return { form, draftChanges, stagedKeys, oauthRequests, clears };
}

describe("ProviderFormComponent", () => {
	beforeAll(() => initTheme("dark"));
	beforeEach(() => setKeybindings(new KeybindingsManager()));

	it("renders the basic provider fields and keeps a detached edit draft", () => {
		const initial: ProviderFormDraft = {
			id: "local",
			provider: {
				name: "Local",
				baseUrl: "http://localhost:8080/v1",
				api: "openai-completions",
				models: [{ id: "one" }],
			},
		};
		const { form } = createForm(initial);

		const output = render(form);
		expect(output).toContain("Template");
		expect(output).toContain("Provider ID  local");
		expect(output).toContain("Name  Local");
		expect(output).toContain("Base URL  http://localhost:8080/v1");
		expect(output).toContain("API protocol  OpenAI Compatible · Chat Completions");
		expect(output).toContain("API key  Not configured");
		expect(output).toContain("Models  1 configured");
		expect(output).toContain("Model · one  Edit");
		expect(output).toContain("Advanced settings  Collapsed");
		expect(form.getDraft()).not.toBe(initial);
		expect(initial.provider.models).toEqual([{ id: "one" }]);
	});

	it("filters templates and fixes the provider ID for a selected preset", () => {
		const { form, draftChanges } = createForm();
		form.handleInput("\r");
		form.handleInput("open");
		expect(render(form)).toContain("OpenAI");
		form.handleInput("\r");

		expect(form.getDraft()).toEqual({
			id: "openai",
			provider: { name: "OpenAI", baseUrl: "https://api.openai.com/v1", api: "openai-responses", models: [] },
		});
		expect(render(form)).toContain("Provider ID  openai (fixed by template)");
		expect(draftChanges).toHaveLength(1);
	});

	it("uses a constrained protocol picker instead of accepting arbitrary protocol text", () => {
		const { form } = createForm();
		for (let index = 0; index < 4; index++) form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(render(form)).toContain("Choose the Provider request format");
		expect(render(form)).toContain("Anthropic · Messages");

		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(form.getDraft().provider.api).toBe("openai-responses");
		expect(render(form)).toContain("API protocol  OpenAI · Responses");
	});

	it("lets a parent consume Escape only after inline state is closed", () => {
		const { form } = createForm();
		form.handleInput("\r");
		expect(render(form)).toContain("OpenAI");
		expect(form.closeTransientState()).toBe(true);
		expect(form.closeTransientState()).toBe(false);

		form.handleInput("\x1b[B");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(render(form)).toContain("> Local");
		expect(form.closeTransientState()).toBe(true);
		expect(form.closeTransientState()).toBe(false);
	});

	it("renders inline inputs and stages API key, OAuth, and clear actions without persistence", () => {
		const { form, stagedKeys, oauthRequests, clears } = createForm();
		form.handleInput("\x1b[B");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		form.handleInput(" Updated");
		form.handleInput("\r");
		expect(form.getDraft().provider.name).toBe("UpdatedLocal");

		form.handleInput("\x1b[B");
		form.handleInput("\x1b[B");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		form.handleInput("secret");
		form.handleInput("\r");
		expect(stagedKeys).toEqual([{ providerId: "local", apiKey: "secret" }]);
		expect(render(form)).toContain("API key  Staged");

		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(oauthRequests).toEqual(["local"]);
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(clears).toEqual(["local"]);
		expect(render(form)).toContain("Cleared (staged)");
	});

	it("adds manual and selected fetched models after filtering the fetched list", async () => {
		let fetchDraft: ProviderFormDraft | undefined;
		let fetchKey: string | undefined;
		const { form } = createForm(
			{
				id: "local",
				provider: {
					name: "Local",
					baseUrl: "http://localhost:8080/v1",
					api: "openai-completions",
					authHeader: false,
					models: [],
				},
			},
			async (draft, stagedApiKey) => {
				fetchDraft = draft;
				fetchKey = stagedApiKey;
				return [
					{ id: "alpha", name: "Alpha" },
					{ id: "beta", name: "Beta" },
				];
			},
		);

		for (let index = 0; index < 9; index++) form.handleInput("\x1b[B");
		form.handleInput("\r");
		form.handleInput("manual-model");
		form.handleInput("\r");
		expect(form.getDraft().provider.models).toEqual([{ id: "manual-model" }]);

		form.handleInput("\x1b[B");
		form.handleInput("\r");
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(fetchDraft?.id).toBe("local");
		expect(fetchKey).toBeUndefined();
		expect(render(form)).toContain("Filter fetched models");

		form.handleInput("beta");
		expect(render(form)).toContain("beta");
		expect(render(form)).not.toContain("alpha");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(render(form)).toContain("beta  Selected");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(form.getDraft().provider.models).toEqual([{ id: "manual-model" }, { id: "beta", name: "Beta" }]);
	});

	it("explains why model discovery is unavailable before it issues a request", async () => {
		let called = false;
		const { form } = createForm(undefined, async () => {
			called = true;
			return [];
		});

		expect(render(form)).toContain("Fetch models  Enter Base URL first");
		for (let index = 0; index < 10; index++) form.handleInput("\x1b[B");
		form.handleInput("\r");
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect(called).toBe(false);
		expect(render(form)).toContain("Enter Base URL first");
	});

	it("edits a selected model inline, including its advanced tuning fields", () => {
		const { form } = createForm({
			id: "local",
			provider: { api: "openai-completions", models: [{ id: "alpha" }] },
		});

		for (let index = 0; index < 9; index++) form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(render(form)).toContain("Editing model alpha within this Provider form");
		expect(render(form)).toContain("Display name  Not set");

		form.handleInput("\x1b[B");
		form.handleInput("\r");
		form.handleInput("Alpha display");
		form.handleInput("\r");
		expect(form.getDraft().provider.models).toContainEqual({ id: "alpha", name: "Alpha display" });

		for (let index = 0; index < 7; index++) form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(render(form)).toContain("Model advanced settings  Expanded");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		form.handleInput("low");
		form.handleInput("\r");
		expect(form.getDraft().provider.models?.[0]?.thinkingLevelMap?.off).toBe("low");
	});

	it("keeps advanced settings inside the Provider form", () => {
		const { form } = createForm();
		for (let index = 0; index < 11; index++) form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(render(form)).toContain("Advanced settings  Expanded");
		expect(render(form)).toContain("Authorization header  Automatic");
		expect(render(form)).toContain("OAuth implementation  Off");
		form.handleInput("\x1b[B");
		form.handleInput("\r");
		expect(form.getDraft().provider.authHeader).toBe(true);
	});

	it("edits shared headers as an inline key-value table without exposing API keys", () => {
		const { form } = createForm();
		for (let index = 0; index < 11; index++) form.handleInput("\x1b[B");
		form.handleInput("\r"); // Advanced settings
		for (let index = 0; index < 3; index++) form.handleInput("\x1b[B");
		form.handleInput("\r"); // Shared headers
		for (let index = 0; index < 3; index++) form.handleInput("\x1b[B");
		form.handleInput("\r"); // Add shared header
		form.handleInput("x-client");
		form.handleInput("\r");
		form.handleInput("bone");
		form.handleInput("\r");

		expect(form.getDraft().provider.headers).toEqual({ "x-client": "bone" });
		expect(render(form)).toContain("x-client  Edit value");
	});
});
