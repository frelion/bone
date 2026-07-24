import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OpenTUIInlinePrompt } from "../src/modes/interactive/components/opentui-inline-prompt.ts";

const renderers = new Set<TestRendererSetup>();

function key(name: string, modifiers: { ctrl?: boolean } = {}): KeyEvent {
	return new KeyEvent({
		name,
		ctrl: modifiers.ctrl ?? false,
		meta: false,
		shift: false,
		option: false,
		sequence: "",
		number: false,
		raw: "",
		eventType: "press",
		source: "raw",
	});
}

async function createPrompt(options: { secret?: boolean; multiline?: boolean } = {}) {
	const setup = await createTestRenderer({ width: 84, height: 18, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	const done = vi.fn<(value: string | undefined) => void>();
	const prompt = new OpenTUIInlinePrompt(
		setup.renderer,
		{ title: options.secret ? "API key" : "Conversation name", ...options },
		done,
	);
	setup.renderer.root.add(prompt.root);
	prompt.focus();
	return { setup, prompt, done };
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUIInlinePrompt", () => {
	test("submits ordinary input with Enter", async () => {
		const { setup, prompt, done } = await createPrompt();
		await setup.mockInput.typeText("Release work");
		prompt.handleKey(key("enter"));
		expect(done).toHaveBeenCalledWith("Release work");
	});

	test("does not render or expose secret input as selectable text", async () => {
		const { setup, prompt, done } = await createPrompt({ secret: true });
		await setup.mockInput.typeText("sk-private-value");
		await setup.flush();
		const frame = setup.captureCharFrame();
		expect(frame).toContain("Secret input · 16 characters");
		expect(frame).not.toContain("sk-private-value");
		prompt.handleKey(key("enter"));
		expect(done).toHaveBeenCalledWith("sk-private-value");
	});

	test("uses Ctrl+S for multiline submission and Escape for cancellation", async () => {
		const first = await createPrompt({ multiline: true });
		await first.setup.mockInput.typeText("first\nsecond");
		first.prompt.handleKey(key("s", { ctrl: true }));
		expect(first.done).toHaveBeenCalledWith("first\nsecond");

		const second = await createPrompt();
		second.prompt.handleKey(key("escape"));
		expect(second.done).toHaveBeenCalledWith(undefined);
	});
});
