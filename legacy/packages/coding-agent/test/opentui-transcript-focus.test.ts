import { BoxRenderable, ScrollBoxRenderable, TextRenderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OpenTUITranscriptFocusController } from "../src/modes/interactive/components/opentui-transcript-focus.ts";

const renderers = new Set<TestRendererSetup>();

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUI transcript focus", () => {
	test("shares sticky follow state between keyboard and mouse scrolling", async () => {
		const setup = await createTestRenderer({ width: 80, height: 14 });
		renderers.add(setup);
		const { renderer } = setup;
		const transcript = new ScrollBoxRenderable(renderer, { width: "100%", height: "100%" });
		const content = new BoxRenderable(renderer, { width: "100%", flexDirection: "column" });
		transcript.add(content);
		renderer.root.add(transcript);
		for (let index = 0; index < 40; index++) {
			content.add(new TextRenderable(renderer, { content: `line-${index}`, height: 1 }));
		}
		await setup.flush();

		const controller = new OpenTUITranscriptFocusController(transcript, () => renderer.height);
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

		// Native ScrollBox dispatches onMouseScroll before it applies its own
		// delta, so the owner predicts the post-event position explicitly.
		controller.handleNativeMouseScroll("up", 3);
		expect(controller.isAutoFollowing()).toBe(false);
		controller.handleNativeMouseScroll("down", Number.MAX_SAFE_INTEGER);
		expect(controller.isAutoFollowing()).toBe(true);
	});

	test("counts semantic updates while paused and clears them when returning to latest", async () => {
		const setup = await createTestRenderer({ width: 80, height: 14 });
		renderers.add(setup);
		const { renderer } = setup;
		const transcript = new ScrollBoxRenderable(renderer, { width: "100%", height: "100%" });
		const content = new BoxRenderable(renderer, { width: "100%", flexDirection: "column" });
		transcript.add(content);
		renderer.root.add(transcript);
		for (let index = 0; index < 40; index++) {
			content.add(new TextRenderable(renderer, { content: `line-${index}`, height: 1 }));
		}
		await setup.flush();

		const controller = new OpenTUITranscriptFocusController(transcript, () => renderer.height);
		const states = vi.fn();
		controller.onStateChange = states;
		controller.followLatest();
		controller.recordSemanticUpdate("content");
		expect(controller.getState()).toEqual({
			following: true,
			unseenUpdateCount: 0,
			latestUpdateKind: undefined,
		});

		controller.scrollByUser(-5);
		controller.recordSemanticUpdate("tool");
		controller.recordSemanticUpdate("completion");
		expect(controller.getState()).toEqual({
			following: false,
			unseenUpdateCount: 2,
			latestUpdateKind: "completion",
		});
		expect(states).toHaveBeenLastCalledWith(controller.getState());

		controller.handleNativeMouseScroll("down", Number.MAX_SAFE_INTEGER);
		expect(controller.getState()).toEqual({
			following: true,
			unseenUpdateCount: 0,
			latestUpdateKind: undefined,
		});

		controller.scrollByUser(-5);
		controller.recordSemanticUpdate();
		controller.jumpToLatest();
		expect(controller.getState().unseenUpdateCount).toBe(0);
	});

	test("preserves near-oldest pagination while tracking paused updates", async () => {
		const setup = await createTestRenderer({ width: 80, height: 14 });
		renderers.add(setup);
		const { renderer } = setup;
		const transcript = new ScrollBoxRenderable(renderer, { width: "100%", height: "100%" });
		const content = new BoxRenderable(renderer, { width: "100%", flexDirection: "column" });
		transcript.add(content);
		renderer.root.add(transcript);
		for (let index = 0; index < 40; index++) {
			content.add(new TextRenderable(renderer, { content: `line-${index}`, height: 1 }));
		}
		await setup.flush();

		const controller = new OpenTUITranscriptFocusController(transcript, () => renderer.height);
		const nearOldest = vi.fn();
		controller.onNearOldestContent = nearOldest;
		controller.followLatest();
		controller.scrollByUser(-Number.MAX_SAFE_INTEGER);

		expect(nearOldest).toHaveBeenCalledOnce();
		expect(controller.isAutoFollowing()).toBe(false);
	});
});
