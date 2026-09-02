import type { Renderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { createEditToolDefinition } from "../src/core/tools/edit.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const setups = new Set<TestRendererSetup>();

afterEach(() => {
	for (const setup of setups) setup.renderer.destroy();
	setups.clear();
});

async function frame(setup: TestRendererSetup, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const captured = setup.captureCharFrame();
		if (captured.includes(expected)) return captured;
	}
	return setup.captureCharFrame();
}

function resolveView(
	view: Renderable | ((renderer: TestRendererSetup["renderer"]) => Renderable),
	setup: TestRendererSetup,
): Renderable {
	return typeof view === "function" ? view(setup.renderer) : view;
}

describe("edit tool OpenTUI rendering", () => {
	beforeAll(() => initTheme("dark"));

	it("renders a settled edit with a native structured diff", async () => {
		const setup = await createTestRenderer({ width: 90, height: 24, autoFocus: false });
		setups.add(setup);
		const { renderer } = setup;
		const definition = createEditToolDefinition("/tmp");
		const renderV2 = definition.renderV2;
		expect(renderV2).toBeDefined();
		const state = {};
		const view = renderV2!.renderResult!(
			{
				result: {
					content: [{ type: "text", text: "Successfully replaced 1 block" }],
					details: {
						diff: "--- a/example.ts\n+++ b/example.ts\n@@ -1 +1 @@\n-old value\n+new value",
						patch: "",
						firstChangedLine: 1,
					},
				},
				isPartial: false,
				expanded: true,
			},
			{
				toolCallId: "edit-1",
				args: { path: "example.ts", edits: [{ oldText: "old value", newText: "new value" }] },
				state,
				cwd: "/tmp",
				executionStarted: true,
				argsComplete: true,
				isPartial: false,
				expanded: true,
				isError: false,
			},
		);
		renderer.root.add(resolveView(view, setup));
		const captured = await frame(setup, "new value");
		expect(captured).toContain("edit");
		expect(captured).toContain("old value");
		expect(captured).not.toContain("Successfully replaced");
	});

	it("renders tool errors without a diff", async () => {
		const setup = await createTestRenderer({ width: 80, height: 16, autoFocus: false });
		setups.add(setup);
		const { renderer } = setup;
		const renderV2 = createEditToolDefinition("/tmp").renderV2!;
		const view = renderV2.renderResult!(
			{
				result: { content: [{ type: "text", text: "Could not find exact text" }], details: undefined },
				isPartial: false,
				expanded: false,
			},
			{
				toolCallId: "edit-2",
				args: { path: "missing.ts", edits: [] },
				state: {},
				cwd: "/tmp",
				executionStarted: true,
				argsComplete: true,
				isPartial: false,
				expanded: false,
				isError: true,
			},
		);
		renderer.root.add(resolveView(view, setup));
		expect(await frame(setup, "Could not find exact text")).not.toContain("@@");
	});
});
