import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test } from "vitest";
import { OpenTUITopBar, OpenTUIWelcome } from "../src/modes/interactive/components/opentui-chrome.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const setups = new Set<TestRendererSetup>();

afterEach(() => {
	for (const setup of setups) setup.renderer.destroy();
	setups.clear();
});

describe("OpenTUI chrome", () => {
	test("removes the branded conversation top bar without spending a terminal row", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		setups.add(setup);
		const topBar = new OpenTUITopBar(setup.renderer, {
			conversation: "Current conversation",
			workspace: "bone",
			model: "openai/gpt-5",
			thinking: "high",
		});
		setup.renderer.root.add(topBar.root);
		await setup.flush();

		const frame = setup.captureCharFrame();
		expect(frame).not.toContain("BONE");
		expect(frame).not.toContain("Current conversation");
		expect(frame.split("\n").filter((line) => line.trim())).toHaveLength(0);
	});

	test("shows a restrained empty-session welcome once and dismisses it permanently", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		setups.add(setup);
		const welcome = new OpenTUIWelcome(setup.renderer, { workspace: "~/src/bone" });
		setup.renderer.root.add(welcome.root);
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("What would you like to work on?");
		expect(setup.captureCharFrame()).toContain("~/src/bone");

		welcome.dismiss();
		await setup.flush();
		expect(setup.captureCharFrame()).not.toContain("What would you like to work on?");
		expect(() => welcome.dismiss()).not.toThrow();
	});
});
