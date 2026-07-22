import { type BoneTestRenderer, createBoneTestRenderer } from "@frelion/bone-tui";
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

const renderers = new Set<BoneTestRenderer>();

async function createRenderer(width = 100, height = 28): Promise<BoneTestRenderer> {
	const renderer = await createBoneTestRenderer({ width, height });
	renderers.add(renderer);
	renderer.start();
	return renderer;
}

async function flushUntil(renderer: BoneTestRenderer, text: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const frame = renderer.captureFrame();
		if (frame.includes(text)) return frame;
	}
	return renderer.captureFrame();
}

beforeEach(() => initTheme("dark"));

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUI dialog and selector v2 flows", () => {
	test("filters and selects a model through structured input and caller actions", async () => {
		const renderer = await createRenderer();
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
		renderer.mount(selector);
		const initial = await flushUntil(renderer, "Follow Conversation");
		expect(initial).toContain("Select model");
		expect(initial).toContain("claude-opus");

		await renderer.input.typeText("gpt");
		const filtered = await flushUntil(renderer, "gpt-5");
		expect(filtered).not.toContain("claude-opus");
		selector.handleAction("confirm");
		expect(selected).toHaveBeenCalledWith({ kind: "model", provider: "openai", id: "gpt-5" });
	});

	test("supports theme preview, thinking, image, and trust product flows", async () => {
		const renderer = await createRenderer();
		const preview = vi.fn();
		const selectTheme = vi.fn();
		const themeSelector = new OpenTUIThemeSelectorV2({
			currentTheme: "dark",
			themes: ["dark", "light"],
			onSelect: selectTheme,
			onCancel: vi.fn(),
			onPreview: preview,
		});
		renderer.mount(themeSelector);
		expect(await flushUntil(renderer, "light")).toContain("Select theme");
		themeSelector.handleAction("down");
		expect(preview).toHaveBeenCalledWith("light");
		themeSelector.handleAction("confirm");
		expect(selectTheme).toHaveBeenCalledWith("light");

		renderer.content.clear();
		const selectThinking = vi.fn();
		const thinking = new OpenTUIThinkingSelectorV2({
			currentLevel: "medium",
			availableLevels: ["off", "medium", "high"],
			onSelect: selectThinking,
			onCancel: vi.fn(),
		});
		renderer.mount(thinking);
		expect(await flushUntil(renderer, "Moderate reasoning")).toContain("Thinking level");
		thinking.handleAction("down");
		thinking.handleAction("confirm");
		expect(selectThinking).toHaveBeenCalledWith("high");

		renderer.content.clear();
		const selectImages = vi.fn();
		const images = new OpenTUIShowImagesSelectorV2({
			currentValue: false,
			onSelect: selectImages,
			onCancel: vi.fn(),
		});
		renderer.mount(images);
		expect(await flushUntil(renderer, "Show text placeholder instead")).toContain("Show images");
		images.handleAction("up");
		images.handleAction("confirm");
		expect(selectImages).toHaveBeenCalledWith(true);

		renderer.content.clear();
		const selectTrust = vi.fn();
		const trust = new OpenTUITrustSelectorV2({
			cwd: "/workspace/project",
			savedDecision: null,
			projectTrusted: false,
			onSelect: selectTrust,
			onCancel: vi.fn(),
		});
		renderer.mount(trust);
		expect(await flushUntil(renderer, "Current workspace: untrusted")).toContain("Project trust");
		trust.handleAction("confirm");
		expect(selectTrust).toHaveBeenCalledWith(expect.objectContaining({ trusted: true, updates: expect.any(Array) }));
	});

	test("renders settings rows and validates structured form values", async () => {
		const renderer = await createRenderer();
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
		renderer.mount(settings);
		expect(await flushUntil(renderer, "Images")).toContain("Settings");
		settings.handleAction("down");
		settings.handleAction("confirm");
		expect(activate).toHaveBeenCalledWith("images");

		renderer.content.clear();
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
		renderer.mount(form);
		await renderer.input.typeText("Example");
		form.handleAction("down");
		await renderer.input.typeText("https://api.example.com");
		form.handleAction("confirm");
		expect(submit).toHaveBeenCalledWith({ name: "Example", url: "https://api.example.com" });
	});

	test("renders login device flow and resolves manual input", async () => {
		const renderer = await createRenderer();
		const complete = vi.fn();
		const login = new OpenTUILoginDialogV2({ providerId: "github", onComplete: complete });
		renderer.mount(login);
		login.showDeviceCode({
			verificationUri: "https://github.com/login/device",
			userCode: "ABCD-1234",
		});
		const deviceFrame = await flushUntil(renderer, "ABCD-1234");
		expect(deviceFrame).toContain("Login to github");
		expect(deviceFrame).toContain("https://github.com/login/device");

		const response = login.showPrompt("Paste the callback code", "code");
		await renderer.input.typeText("verified");
		login.handleAction("confirm");
		await expect(response).resolves.toBe("verified");
		expect(complete).not.toHaveBeenCalled();
	});

	test("uses a readable single-column settings layout on narrow terminals", async () => {
		const renderer = await createRenderer(60, 18);
		const settings = new OpenTUISettingsListViewV2({
			title: "Settings",
			items: [
				{ id: "model", label: "Model", value: "Select the active model" },
				{ id: "thinking", label: "Thinking level", value: "Set reasoning effort" },
			],
			onActivate: vi.fn(),
			onCancel: vi.fn(),
		});
		renderer.mount(settings);
		const captured = await flushUntil(renderer, "Set reasoning effort");

		expect(captured).toContain("Thinking level");
		expect(captured).not.toContain("levelSet reasoning");
		expect(Math.max(...captured.split("\n").map((line) => line.length))).toBeLessThanOrEqual(60);
	});
});
