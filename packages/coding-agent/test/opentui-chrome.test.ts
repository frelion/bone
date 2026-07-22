import { type BoneTestRenderer, createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, describe, expect, test } from "vitest";
import { OpenTUITopBar, OpenTUIWelcome } from "../src/modes/interactive/components/opentui-chrome.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<BoneTestRenderer>();

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUI chrome", () => {
	test("removes the branded conversation top bar without spending a terminal row", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 80, height: 12 });
		renderers.add(renderer);
		renderer.start();
		const topBar = new OpenTUITopBar({
			conversation: "Current conversation",
			workspace: "bone",
			model: "openai/gpt-5",
			thinking: "high",
		});
		renderer.mount(topBar);
		await renderer.flush();

		const frame = renderer.captureFrame();
		expect(frame).not.toContain("BONE");
		expect(frame).not.toContain("Current conversation");
		expect(frame.split("\n").filter((line) => line.trim())).toHaveLength(0);
	});

	test("shows a restrained empty-session welcome once and dismisses it permanently", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 80, height: 12 });
		renderers.add(renderer);
		renderer.start();
		const welcome = new OpenTUIWelcome({ workspace: "~/src/bone" });
		renderer.mount(welcome);
		await renderer.flush();
		expect(renderer.captureFrame()).toContain("What would you like to work on?");
		expect(renderer.captureFrame()).toContain("~/src/bone");

		welcome.dismiss();
		await renderer.flush();
		expect(renderer.captureFrame()).not.toContain("What would you like to work on?");
		expect(() => welcome.dismiss()).not.toThrow();
	});
});
