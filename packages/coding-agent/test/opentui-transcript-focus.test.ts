import { type BoneView, createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OpenTUITranscriptFocusController } from "../src/modes/interactive/components/opentui-transcript-focus.ts";
import { OpenTUIInteractiveShell } from "../src/modes/interactive/opentui-shell.ts";

const renderers = new Set<Awaited<ReturnType<typeof createBoneTestRenderer>>>();

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

function line(content: string): BoneView {
	return { mount: (context) => context.createText({ content }) };
}

describe("OpenTUI transcript focus", () => {
	test("shares sticky follow state between keyboard and mouse scrolling", async () => {
		const renderer = await createBoneTestRenderer({ width: 80, height: 14 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell();
		renderer.mount(shell);
		for (let index = 0; index < 40; index++) shell.appendTranscript(line(`line-${index}`));
		await renderer.flush();

		const controller = new OpenTUITranscriptFocusController(shell.getTranscriptNode(), () => renderer.height);
		const changes = vi.fn();
		controller.onAutoFollowChange = changes;
		controller.followLatest();
		expect(controller.isAutoFollowing()).toBe(true);

		controller.scrollByUser(-5);
		expect(controller.isAutoFollowing()).toBe(false);
		expect(changes).toHaveBeenLastCalledWith(false);

		controller.scrollByUser(Number.MAX_SAFE_INTEGER);
		expect(controller.isAutoFollowing()).toBe(true);
		expect(changes).toHaveBeenLastCalledWith(true);

		controller.handleMouseScroll(-3);
		expect(controller.isAutoFollowing()).toBe(false);
		controller.followLatest();
		expect(controller.isAutoFollowing()).toBe(true);
	});
});
