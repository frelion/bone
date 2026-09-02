import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OpenTUIQuickPicker } from "../src/modes/interactive/components/opentui-quick-picker.ts";

const renderers = new Set<TestRendererSetup>();

async function createQuickPickerRenderer() {
	const setup = await createTestRenderer({ width: 84, height: 20, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	return setup;
}

function key(name: string): KeyEvent {
	return new KeyEvent({
		name,
		ctrl: false,
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

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUIQuickPicker", () => {
	test("selects the initial value and supports keyboard cancellation", async () => {
		const setup = await createQuickPickerRenderer();
		const done = vi.fn<(value: string | undefined) => void>();
		const picker = new OpenTUIQuickPicker(
			setup.renderer,
			{
				title: "Choose provider",
				options: [
					{ value: "first", label: "First" },
					{ value: "second", label: "Second" },
				],
				initialValue: "second",
			},
			done,
		);
		setup.renderer.root.add(picker.root);
		picker.handleKey(key("enter"));
		expect(done).toHaveBeenCalledWith("second");

		const cancelled = vi.fn<(value: string | undefined) => void>();
		const cancelPicker = new OpenTUIQuickPicker(
			setup.renderer,
			{ title: "Choose provider", options: [{ value: "first", label: "First" }] },
			cancelled,
		);
		cancelPicker.handleKey(key("escape"));
		expect(cancelled).toHaveBeenCalledWith(undefined);
	});

	test("filters searchable options without losing conversation context", async () => {
		const setup = await createQuickPickerRenderer();
		const done = vi.fn<(value: string | undefined) => void>();
		const picker = new OpenTUIQuickPicker(
			setup.renderer,
			{
				title: "Select model",
				searchable: true,
				searchPlaceholder: "Search models",
				options: [
					{ value: "alpha", label: "Alpha", description: "provider-a/alpha" },
					{ value: "gamma", label: "Gamma", description: "provider-b/gamma" },
				],
			},
			done,
		);
		setup.renderer.root.add(picker.root);
		picker.focus();
		await setup.mockInput.typeText("provider-b");
		await setup.flush();
		const frame = setup.captureCharFrame();
		expect(frame).toContain("Gamma");
		expect(frame).not.toContain("Alpha");

		setup.mockInput.pressEnter();
		expect(done).toHaveBeenCalledWith("gamma");
	});

	test("shows an empty state and does not select disabled options", async () => {
		const setup = await createQuickPickerRenderer();
		const done = vi.fn<(value: string | undefined) => void>();
		const picker = new OpenTUIQuickPicker(
			setup.renderer,
			{
				title: "Select model",
				searchable: true,
				options: [{ value: "disabled", label: "Disabled", disabled: true }],
			},
			done,
		);
		setup.renderer.root.add(picker.root);
		picker.focus();
		picker.handleKey(key("enter"));
		expect(done).not.toHaveBeenCalled();
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Disabled (unavailable)");
		await setup.mockInput.typeText("missing");
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("No matching options");
		picker.handleKey(key("enter"));
		expect(done).not.toHaveBeenCalled();
	});
});
