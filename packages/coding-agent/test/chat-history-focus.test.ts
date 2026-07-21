import { setKeybindings } from "@frelion/bone-tui";
import { describe, expect, it, vi } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import { ChatHistoryFocus } from "../src/modes/interactive/components/chat-history-focus.ts";

describe("ChatHistoryFocus", () => {
	it("uses line scrolling for arrows and page scrolling for PageUp/PageDown", () => {
		setKeybindings(new KeybindingsManager());
		const history = new ChatHistoryFocus();
		const onScroll = vi.fn();
		history.onScroll = onScroll;

		history.handleInput("\x1b[A");
		history.handleInput("\x1b[B");
		history.handleInput("\x1b[5~");
		history.handleInput("\x1b[6~");

		expect(onScroll).toHaveBeenNthCalledWith(1, "up", "line");
		expect(onScroll).toHaveBeenNthCalledWith(2, "down", "line");
		expect(onScroll).toHaveBeenNthCalledWith(3, "up", "page");
		expect(onScroll).toHaveBeenNthCalledWith(4, "down", "page");
	});
});
