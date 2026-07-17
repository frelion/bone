import { setKeybindings } from "@earendil-works/pi-tui";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import { ChatHistoryFocus } from "../src/modes/interactive/components/chat-history-focus.ts";

beforeEach(() => {
	setKeybindings(new KeybindingsManager());
});

describe("ChatHistoryFocus", () => {
	it("uses Shift+arrows for pane focus while ordinary keys scroll only the transcript", () => {
		const history = new ChatHistoryFocus();
		const focusSidebar = vi.fn();
		const focusComposer = vi.fn();
		const scroll = vi.fn();
		history.onFocusSidebar = focusSidebar;
		history.onFocusComposer = focusComposer;
		history.onScroll = scroll;

		history.handleInput("\x1b[d"); // Shift+Left
		history.handleInput("\x1b[b"); // Shift+Down
		history.handleInput("\x1b[A"); // Up
		history.handleInput("\x1b[6~"); // PageDown

		expect(focusSidebar).toHaveBeenCalledTimes(1);
		expect(focusComposer).toHaveBeenCalledTimes(1);
		expect(scroll).toHaveBeenNthCalledWith(1, "up");
		expect(scroll).toHaveBeenNthCalledWith(2, "down");
	});
});
