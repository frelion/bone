import type { AgentMessage } from "@frelion/bone-agent-core";
import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { SessionEntry, SessionTreeNode } from "../src/core/session-manager.ts";
import { OpenTUIHistoryNavigator } from "../src/modes/interactive/components/opentui-history-navigator.ts";

const renderers = new Set<TestRendererSetup>();

function message(id: string, parentId: string | null, role: "user" | "assistant", content: string): SessionEntry {
	return {
		type: "message",
		id,
		parentId,
		timestamp: new Date(0).toISOString(),
		message: { role, content, timestamp: 1 } as AgentMessage,
	};
}

function tree(): SessionTreeNode[] {
	const first = message("user-1", null, "user", "Start the migration");
	const assistant = message("assistant-1", first.id, "assistant", "I inspected the renderer.");
	const second = message("user-2", assistant.id, "user", "Keep the public API stable");
	return [
		{
			entry: first,
			children: [{ entry: assistant, children: [{ entry: second, children: [], label: "API boundary" }] }],
		},
	];
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

async function setupNavigator(mode: "tree" | "fork", currentLeafId = "user-2") {
	const setup = await createTestRenderer({ width: 90, height: 26, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	const done = vi.fn<(entryId: string | undefined) => void>();
	const navigator = new OpenTUIHistoryNavigator(setup.renderer, {
		mode,
		tree: tree(),
		currentLeafId,
		onDone: done,
	});
	setup.renderer.root.add(navigator.root);
	navigator.focus();
	return { setup, navigator, done };
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUIHistoryNavigator", () => {
	test("shows hierarchy, current leaf, labels, and a contextual preview", async () => {
		const { setup } = await setupNavigator("tree");
		await setup.flush();
		const frame = setup.captureCharFrame();
		expect(frame).toContain("Conversation tree");
		expect(frame).toContain("|- assistant");
		expect(frame).toContain("[API boundary] user (current)");
		expect(frame).toContain("Keep the public API stable");
	});

	test("filters message content and selects the matching tree node", async () => {
		const { setup, done } = await setupNavigator("tree");
		await setup.mockInput.typeText("renderer");
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("I inspected the renderer.");
		expect(setup.captureCharFrame()).not.toContain("Start the migration");
		setup.mockInput.pressEnter();
		expect(done).toHaveBeenCalledWith("assistant-1");
	});

	test("keeps non-user context visible but prevents forking from it", async () => {
		const { setup, navigator, done } = await setupNavigator("fork", "assistant-1");
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("assistant (current) (not forkable)");
		navigator.handleKey(key("enter"));
		expect(done).not.toHaveBeenCalled();
		navigator.handleKey(key("down"));
		navigator.handleKey(key("enter"));
		expect(done).toHaveBeenCalledWith("user-2");
	});

	test("returns without navigating on Escape", async () => {
		const { navigator, done } = await setupNavigator("tree");
		navigator.handleKey(key("escape"));
		expect(done).toHaveBeenCalledWith(undefined);
	});
});
