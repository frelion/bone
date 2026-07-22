import { createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { createEditToolDefinition } from "../src/core/tools/edit.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<Awaited<ReturnType<typeof createBoneTestRenderer>>>();

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

async function frame(renderer: Awaited<ReturnType<typeof createBoneTestRenderer>>, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const captured = renderer.captureFrame();
		if (captured.includes(expected)) return captured;
	}
	return renderer.captureFrame();
}

describe("edit tool OpenTUI rendering", () => {
	beforeAll(() => initTheme("dark"));

	it("renders a settled edit with a native structured diff", async () => {
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
		renderers.add(renderer);
		renderer.start();
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
		renderer.content.append(view.mount(renderer));
		const captured = await frame(renderer, "new value");
		expect(captured).toContain("edit");
		expect(captured).toContain("old value");
		expect(captured).not.toContain("Successfully replaced");
	});

	it("renders tool errors without a diff", async () => {
		const renderer = await createBoneTestRenderer({ width: 80, height: 16 });
		renderers.add(renderer);
		renderer.start();
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
		renderer.content.append(view.mount(renderer));
		expect(await frame(renderer, "Could not find exact text")).not.toContain("@@");
	});
});
