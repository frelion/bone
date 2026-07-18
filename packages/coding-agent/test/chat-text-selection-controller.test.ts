import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatScrollLayout } from "../src/modes/interactive/components/chat-scroll-layout.ts";
import {
	ChatTextSelectionController,
	parseSgrMouseEvent,
} from "../src/modes/interactive/components/chat-text-selection-controller.ts";

class StaticComponent {
	private readonly lines: string[];

	constructor(lines: string[]) {
		this.lines = lines;
	}

	invalidate(): void {}

	render(): string[] {
		return this.lines;
	}
}

function makeController(layout: ChatScrollLayout) {
	const onRender = vi.fn();
	const onCopy = vi.fn(async () => {});
	const onCopied = vi.fn();
	const controller = new ChatTextSelectionController({
		layout,
		getBounds: () => ({ left: 3, top: 0, width: 20, height: layout.getVisibleContentRowCount() }),
		isBlocked: () => false,
		onRender,
		onCopy,
		onCopied,
		onCopyError: vi.fn(),
	});
	return { controller, onCopy, onCopied };
}

afterEach(() => {
	vi.useRealTimers();
});

describe("ChatTextSelectionController", () => {
	it("parses SGR drag and wheel events", () => {
		expect(parseSgrMouseEvent("\x1b[<0;4;2M")).toMatchObject({ button: 0, column: 4, row: 2, isRelease: false });
		expect(parseSgrMouseEvent("\x1b[<32;6;2M")).toMatchObject({ isMotion: true });
		expect(parseSgrMouseEvent("\x1b[<65;6;2M")).toMatchObject({ isWheel: true });
		expect(parseSgrMouseEvent("not a mouse event")).toBeUndefined();
	});

	it("keeps wheel events available for chat scrolling", () => {
		const layout = new ChatScrollLayout(new StaticComponent(["hello"]), new StaticComponent([]), () => 4);
		layout.render(20);
		const { controller } = makeController(layout);

		expect(controller.handleInput("\x1b[<64;4;1M")).toBeUndefined();
	});

	it("copies a dragged chat selection and consumes the mouse sequence", async () => {
		const layout = new ChatScrollLayout(new StaticComponent(["hello world"]), new StaticComponent([]), () => 4);
		layout.render(20);
		const { controller, onCopy, onCopied } = makeController(layout);

		expect(controller.handleInput("\x1b[<0;5;1M")).toEqual({ consume: true });
		expect(controller.handleInput("\x1b[<32;8;1M")).toEqual({ consume: true });
		expect(controller.handleInput("\x1b[<0;8;1m")).toEqual({ consume: true });
		await vi.waitFor(() => expect(onCopy).toHaveBeenCalledWith("ello"));
		expect(onCopied).toHaveBeenCalledWith(4);
	});

	it("does not begin a selection from the Side pane", () => {
		const layout = new ChatScrollLayout(new StaticComponent(["hello"]), new StaticComponent([]), () => 4);
		layout.render(20);
		const { controller } = makeController(layout);

		controller.handleInput("\x1b[<0;2;1M");
		expect(layout.textSelection.active).toBe(false);
	});

	it("auto-scrolls while a drag remains above the chat viewport and stops on cancel", () => {
		vi.useFakeTimers();
		const layout = new ChatScrollLayout(
			new StaticComponent(["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]),
			new StaticComponent([]),
			() => 4,
		);
		layout.render(20);
		const onAutoScroll = vi.fn((direction: "up" | "down") => {
			const scrolled = layout.scrollLines(direction);
			layout.render(20);
			return scrolled;
		});
		const controller = new ChatTextSelectionController({
			layout,
			getBounds: () => ({ left: 3, top: 0, width: 20, height: layout.getVisibleContentRowCount() }),
			isBlocked: () => false,
			onRender: vi.fn(),
			onAutoScroll,
			onCopy: async () => {},
			onCopied: vi.fn(),
			onCopyError: vi.fn(),
		});

		controller.handleInput("\x1b[<0;5;4M");
		// A real terminal clamps a pointer above the top edge to row 1.
		controller.handleInput("\x1b[<32;5;1M");
		vi.advanceTimersByTime(120);

		expect(onAutoScroll).toHaveBeenCalledWith("up");
		expect(layout.getScrollOffset()).toBeGreaterThan(1);

		controller.cancel();
		const callsAfterCancel = onAutoScroll.mock.calls.length;
		vi.advanceTimersByTime(200);
		expect(onAutoScroll).toHaveBeenCalledTimes(callsAfterCancel);
	});
});
