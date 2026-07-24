import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test } from "vitest";
import { OpenTUITopBar, OpenTUIWelcome } from "../src/modes/interactive/components/opentui-chrome.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

const setups = new Set<TestRendererSetup>();

afterEach(() => {
	for (const setup of setups) setup.renderer.destroy();
	setups.clear();
});

describe("OpenTUI chrome", () => {
	test("shows conversation and workspace identity in a restrained one-line top bar", async () => {
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
		expect(frame).toContain("Current conversation");
		expect(frame).toContain("bone");
		expect(frame.split("\n").filter((line) => line.trim())).toHaveLength(1);
	});

	test("updates top bar identity and tolerates theme refresh and repeated disposal", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 48, height: 8 });
		setups.add(setup);
		const topBar = new OpenTUITopBar(setup.renderer, {
			conversation: "Initial task",
			workspace: "first-workspace",
			model: "openai/gpt-5",
			thinking: "high",
		});
		setup.renderer.root.add(topBar.root);
		topBar.update({
			conversation: "Updated task",
			workspace: "bone",
			model: "openai/gpt-5",
			thinking: "medium",
		});
		initTheme("light");
		topBar.updateTheme(theme);
		await setup.flush();

		const frame = setup.captureCharFrame();
		expect(frame).toContain("Updated task");
		expect(frame).toContain("bone");
		expect(frame).not.toContain("Initial task");
		expect(() => topBar.dispose()).not.toThrow();
		expect(() => topBar.dispose()).not.toThrow();
	});

	test("shows a persistent textual Plan mode marker", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 60, height: 8 });
		setups.add(setup);
		const topBar = new OpenTUITopBar(setup.renderer, {
			conversation: "Migration",
			workspace: "bone",
			model: "openai/gpt-5",
			thinking: "high",
			mode: "plan",
		});
		setup.renderer.root.add(topBar.root);
		await setup.flush();

		expect(setup.captureCharFrame()).toContain("[PLAN] Migration");
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
