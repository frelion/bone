import type { BoneView } from "@frelion/bone-tui";
import { describe, expect, test } from "vitest";
import { createEventBus } from "../src/core/event-bus.ts";
import {
	createExtensionRuntime,
	createExtensionUIV2Context,
	type ExtensionAPI,
	type ExtensionUIToolViewRenderer,
	loadExtensionFromFactory,
} from "../src/core/extensions/index.ts";

function view(content: string): BoneView {
	return {
		mount(context) {
			return context.createText({ content });
		},
	};
}

describe("extension UI v2", () => {
	test("provides a headless structured context without legacy adapters", async () => {
		const ui = createExtensionUIV2Context();

		const selected = await ui.dialogs.select({
			title: "Mode",
			options: [
				{ value: "safe", label: "Safe" },
				{ value: "fast", label: "Fast", disabled: true },
			],
		});
		expect(selected).toBeUndefined();
		expect(ui.available).toBe(false);
		expect(ui.editor.getText()).toBe("");
		ui.editor.setText("replacement");
		ui.editor.insertText("addition");
		ui.dialogs.notify("Saved", "info");

		const handle = ui.widgets.set("status", view("Ready"));
		expect(handle.mounted).toBe(false);
		expect(ui.advanced.createView((context) => context.createText({ content: "Advanced" }))).toBeDefined();
		expect("keybindings" in ui).toBe(false);
		expect("shortcuts" in ui).toBe(false);
	});

	test("does not expose extension-defined keyboard shortcuts", async () => {
		let loadedApi: ExtensionAPI | undefined;
		const extension = await loadExtensionFromFactory(
			(api) => {
				loadedApi = api;
			},
			process.cwd(),
			createEventBus(),
			createExtensionRuntime(),
		);

		expect(loadedApi).toBeDefined();
		expect("registerShortcut" in (loadedApi as ExtensionAPI)).toBe(false);
		expect("shortcuts" in extension).toBe(false);
	});

	test("exposes a typed BoneView tool rendering contract", () => {
		const renderer: ExtensionUIToolViewRenderer<{ path: string }> = {
			renderCall: (args: { path: string }) => view(`Reading ${args.path}`),
			renderResult: ({ result }) =>
				view(result.content.map((part) => (part.type === "text" ? part.text : "image")).join("\n")),
		};

		expect(renderer.renderCall({ path: "README.md" }, {} as never)).toBeDefined();
	});
});
