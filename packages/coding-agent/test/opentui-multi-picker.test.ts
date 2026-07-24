import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OpenTUIMultiPicker } from "../src/modes/interactive/components/opentui-multi-picker.ts";

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

async function setupPicker() {
	const setup = await createTestRenderer({ width: 84, height: 22, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	const done = vi.fn<(values: string[] | undefined) => void>();
	const picker = new OpenTUIMultiPicker(
		setup.renderer,
		{
			title: "Model cycling scope",
			searchable: true,
			searchPlaceholder: "Search models",
			initialValues: ["test/first"],
			options: [
				{ value: "test/first", label: "First", description: "test/first" },
				{ value: "test/second", label: "Second", description: "test/second" },
			],
		},
		done,
	);
	setup.renderer.root.add(picker.root);
	picker.focus();
	return { setup, picker, done };
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUIMultiPicker", () => {
	test("toggles several values and applies them together with Ctrl+S", async () => {
		const { setup, picker, done } = await setupPicker();
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("[x] First");
		expect(setup.captureCharFrame()).toContain("1 selected");
		picker.handleKey(key("down"));
		picker.handleKey(key("enter"));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("[x] Second");
		expect(setup.captureCharFrame()).toContain("2 selected");
		picker.handleKey(key("s", { ctrl: true }));
		expect(done).toHaveBeenCalledWith(["test/first", "test/second"]);
	});

	test("filters without losing selections and discards on Escape", async () => {
		const { setup, picker, done } = await setupPicker();
		await setup.mockInput.typeText("second");
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Second");
		expect(setup.captureCharFrame()).not.toContain("[x] First");
		picker.handleKey(key("enter"));
		picker.handleKey(key("escape"));
		expect(done).toHaveBeenCalledWith(undefined);
	});
});
