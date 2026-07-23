import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { OpenTUILoginDialogV2 } from "../src/modes/interactive/components/opentui-login-dialog-v2.ts";
import {
	OpenTUIModelSelectorV2,
	OpenTUIShowImagesSelectorV2,
	OpenTUIThemeSelectorV2,
	OpenTUIThinkingSelectorV2,
	OpenTUITrustSelectorV2,
} from "../src/modes/interactive/components/opentui-selectors-v2.ts";
import {
	OpenTUIFormViewV2,
	OpenTUISettingsListViewV2,
} from "../src/modes/interactive/components/opentui-settings-v2.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const setups = new Set<TestRendererSetup>();

async function createRenderer(width = 100, height = 28): Promise<TestRendererSetup> {
	const setup = await createTestRenderer({ width, height, autoFocus: false, useMouse: true });
	setups.add(setup);
	setup.renderer.start();
	return setup;
}

async function flushUntil(setup: TestRendererSetup, text: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const frame = setup.captureCharFrame();
		if (frame.includes(text)) return frame;
	}
	return setup.captureCharFrame();
}

beforeEach(() => initTheme("dark"));

afterEach(() => {
	for (const setup of setups) setup.renderer.destroy();
	setups.clear();
});

describe("OpenTUI dialog and selector v2 flows", () => {
	test("filters and selects a model through structured input and caller actions", async () => {
		const setup = await createRenderer();
		const { renderer, mockInput } = setup;
		const selected = vi.fn();
		const selector = new OpenTUIModelSelectorV2({
			models: [
				{ provider: "anthropic", id: "claude-opus", name: "Claude Opus", current: true },
				{ provider: "openai", id: "gpt-5", name: "GPT 5" },
			],
			allowFollowConversation: true,
			onSelect: selected,
			onCancel: vi.fn(),
		});
		renderer.root.add(selector.build(renderer));
		selector.focus();
		const initial = await flushUntil(setup, "Follow Conversation");
		expect(initial).toContain("Select model");
		expect(initial).toContain("claude-opus");

		await mockInput.typeText("gpt");
		const filtered = await flushUntil(setup, "gpt-5");
		expect(filtered).not.toContain("claude-opus");
		selector.handleAction("confirm");
		expect(selected).toHaveBeenCalledWith({ kind: "model", provider: "openai", id: "gpt-5" });
	});

	test("supports theme preview, thinking, image, and trust product flows", async () => {
		const setup = await createRenderer();
		const { renderer } = setup;
		const preview = vi.fn();
		const selectTheme = vi.fn();
		const themeSelector = new OpenTUIThemeSelectorV2({
			currentTheme: "dark",
			themes: ["dark", "light"],
			onSelect: selectTheme,
			onCancel: vi.fn(),
			onPreview: preview,
		});
		renderer.root.add(themeSelector.build(renderer));
		themeSelector.focus();
		expect(await flushUntil(setup, "light")).toContain("Select theme");
		themeSelector.handleAction("down");
		expect(preview).toHaveBeenCalledWith("light");
		themeSelector.handleAction("confirm");
		expect(selectTheme).toHaveBeenCalledWith("light");

		for (const child of renderer.root.getChildren()) child.destroyRecursively();
		const selectThinking = vi.fn();
		const thinking = new OpenTUIThinkingSelectorV2({
			currentLevel: "medium",
			availableLevels: ["off", "medium", "high"],
			onSelect: selectThinking,
			onCancel: vi.fn(),
		});
		renderer.root.add(thinking.build(renderer));
		thinking.focus();
		expect(await flushUntil(setup, "Moderate reasoning")).toContain("Thinking level");
		thinking.handleAction("down");
		thinking.handleAction("confirm");
		expect(selectThinking).toHaveBeenCalledWith("high");

		for (const child of renderer.root.getChildren()) child.destroyRecursively();
		const selectImages = vi.fn();
		const images = new OpenTUIShowImagesSelectorV2({
			currentValue: false,
			onSelect: selectImages,
			onCancel: vi.fn(),
		});
		renderer.root.add(images.build(renderer));
		images.focus();
		expect(await flushUntil(setup, "Show text placeholder instead")).toContain("Show images");
		images.handleAction("up");
		images.handleAction("confirm");
		expect(selectImages).toHaveBeenCalledWith(true);

		for (const child of renderer.root.getChildren()) child.destroyRecursively();
		const selectTrust = vi.fn();
		const trust = new OpenTUITrustSelectorV2({
			cwd: "/workspace/project",
			savedDecision: null,
			projectTrusted: false,
			onSelect: selectTrust,
			onCancel: vi.fn(),
		});
		renderer.root.add(trust.build(renderer));
		trust.focus();
		expect(await flushUntil(setup, "Current workspace: untrusted")).toContain("Project trust");
		trust.handleAction("confirm");
		expect(selectTrust).toHaveBeenCalledWith(expect.objectContaining({ trusted: true, updates: expect.any(Array) }));
	});

	test("renders settings rows and validates structured form values", async () => {
		const setup = await createRenderer();
		const { renderer } = setup;
		const activate = vi.fn();
		const settings = new OpenTUISettingsListViewV2({
			title: "Settings",
			items: [
				{ id: "theme", label: "Theme", value: "dark" },
				{ id: "images", label: "Images", value: "off" },
			],
			onActivate: activate,
			onCancel: vi.fn(),
		});
		renderer.root.add(settings.build(renderer));
		settings.focus();
		expect(await flushUntil(setup, "Images")).toContain("Settings");
		settings.handleAction("down");
		settings.handleAction("confirm");
		expect(activate).toHaveBeenCalledWith("images");

		for (const child of renderer.root.getChildren()) child.destroyRecursively();
		const submit = vi.fn();
		const form = new OpenTUIFormViewV2({
			title: "Provider form",
			fields: [
				{ id: "name", label: "Name", required: true },
				{ id: "url", label: "Base URL", placeholder: "https://api.example.com" },
			],
			onSubmit: submit,
			onCancel: vi.fn(),
		});
		renderer.root.add(form.build(renderer));
		form.focus();
		await setup.mockInput.typeText("Example");
		form.handleAction("down");
		await setup.mockInput.typeText("https://api.example.com");
		form.handleAction("confirm");
		expect(submit).toHaveBeenCalledWith({ name: "Example", url: "https://api.example.com" });
	});

	test("renders login device flow and resolves manual input", async () => {
		const setup = await createRenderer();
		const { renderer } = setup;
		const complete = vi.fn();
		const login = new OpenTUILoginDialogV2({ providerId: "github", onComplete: complete });
		renderer.root.add(login.build(renderer));
		login.showDeviceCode({
			verificationUri: "https://github.com/login/device",
			userCode: "ABCD-1234",
		});
		const deviceFrame = await flushUntil(setup, "ABCD-1234");
		expect(deviceFrame).toContain("Login to github");
		expect(deviceFrame).toContain("https://github.com/login/device");

		const response = login.showPrompt("Paste the callback code", "code");
		login.focus();
		await setup.mockInput.typeText("verified");
		login.handleAction("confirm");
		await expect(response).resolves.toBe("verified");
		expect(complete).not.toHaveBeenCalled();
	});

	test("uses a readable single-column settings layout on narrow terminals", async () => {
		const setup = await createRenderer(60, 18);
		const { renderer } = setup;
		const settings = new OpenTUISettingsListViewV2({
			title: "Settings",
			items: [
				{ id: "model", label: "Model", value: "Select the active model" },
				{ id: "thinking", label: "Thinking level", value: "Set reasoning effort" },
			],
			onActivate: vi.fn(),
			onCancel: vi.fn(),
		});
		renderer.root.add(settings.build(renderer));
		const captured = await flushUntil(setup, "Set reasoning effort");

		expect(captured).toContain("Thinking level");
		expect(captured).not.toContain("levelSet reasoning");
		expect(Math.max(...captured.split("\n").map((line) => line.length))).toBeLessThanOrEqual(60);
		expect(captured).not.toContain("fields");
		expect(captured).not.toContain("Esc cancel");
	});
});
