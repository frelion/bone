import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OpenTUISettingsSaveRequested } from "../src/modes/interactive/components/opentui-settings-center.ts";
import { OpenTUISettingsPage } from "../src/modes/interactive/components/opentui-settings-page.ts";

const renderers = new Set<TestRendererSetup>();

async function createSettingsPage() {
	const setup = await createTestRenderer({ width: 90, height: 24, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	const confirm = vi.fn(async () => true);
	const page = new OpenTUISettingsPage(setup.renderer, { confirm, notify: vi.fn() });
	setup.renderer.root.add(page.root);
	return { setup, page, confirm };
}

function key(name: string, modifiers: { ctrl?: boolean; shift?: boolean } = {}): KeyEvent {
	return new KeyEvent({
		name,
		ctrl: modifiers.ctrl ?? false,
		meta: false,
		shift: modifiers.shift ?? false,
		option: false,
		sequence: "",
		number: false,
		raw: "",
		eventType: "press",
		source: "raw",
	});
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUISettingsPage", () => {
	test("renders settings choices in the main page and resolves keyboard selection", async () => {
		const { setup, page } = await createSettingsPage();
		const selection = page.dialogs.select({
			title: "Settings · Global",
			options: [
				{ value: "defaults", label: "Defaults & Sessions" },
				{ value: "appearance", label: "Appearance & Terminal", description: "Theme and terminal rendering" },
			],
		});
		await setup.flush();
		const frame = setup.captureCharFrame();
		expect(frame).toContain("Settings · Global");
		expect(frame).toContain("Appearance & Terminal");
		expect(frame).toContain("Ctrl+S save");

		page.handleKey(key("down"));
		page.handleKey(key("enter"));
		await expect(selection).resolves.toBe("appearance");
	});

	test("applies the current field before Ctrl+S requests a transaction save", async () => {
		const { setup, page } = await createSettingsPage();
		const input = page.dialogs.input({ title: "HTTP proxy" });
		await setup.mockInput.typeText("http://new-proxy");
		page.handleKey(key("s", { ctrl: true }));
		await expect(input).resolves.toBe("http://new-proxy");

		await expect(
			page.dialogs.select({
				title: "Tools, Shell & Network",
				options: [{ value: "httpProxy", label: "HTTP proxy" }],
			}),
		).rejects.toBeInstanceOf(OpenTUISettingsSaveRequested);
	});

	test("keeps destructive confirmation delegated to the modal service", async () => {
		const { page, confirm } = await createSettingsPage();
		await expect(
			page.dialogs.confirm({ title: "Delete provider", message: "Delete provider configuration?" }),
		).resolves.toBe(true);
		expect(confirm).toHaveBeenCalledWith({
			title: "Delete provider",
			message: "Delete provider configuration?",
		});
	});
});
