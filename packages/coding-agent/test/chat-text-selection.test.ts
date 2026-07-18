import { describe, expect, it } from "vitest";
import { ChatScrollLayout } from "../src/modes/interactive/components/chat-scroll-layout.ts";
import { ChatTextSelection } from "../src/modes/interactive/components/chat-text-selection.ts";

class MutableComponent {
	lines: string[];

	constructor(lines: string[]) {
		this.lines = lines;
	}

	invalidate(): void {}

	render(): string[] {
		return this.lines;
	}
}

describe("ChatTextSelection", () => {
	it("copies an inclusive selection on one rendered line", () => {
		const selection = new ChatTextSelection();
		selection.begin(["hello world"], 0, 1);
		selection.update(0, 4);

		expect(selection.copyText()).toBe("ello");
	});

	it("copies across rendered rows without ANSI styling", () => {
		const selection = new ChatTextSelection();
		selection.begin(["\x1b[31malpha\x1b[39m", "bravo", "charlie"], 0, 2);
		selection.update(1, 1);

		expect(selection.copyText()).toBe("pha\nbr");
	});

	it("keeps a wide grapheme whole when the drag crosses either display cell", () => {
		const selection = new ChatTextSelection();
		selection.begin(["A你B"], 0, 1);
		selection.update(0, 2);

		expect(selection.copyText()).toBe("你");
	});

	it("renders the active range with reverse video and preserves surrounding text", () => {
		const selection = new ChatTextSelection();
		selection.begin(["hello"], 0, 1);
		selection.update(0, 3);

		expect(selection.renderLine("hello", 0)).toBe("h\x1b[7mell\x1b[27mo");
	});

	it("does not copy until the pointer has moved and cancels cleanly", () => {
		const selection = new ChatTextSelection();
		selection.begin(["hello"], 0, 2);

		expect(selection.copyText()).toBeUndefined();
		expect(selection.cancel()).toBe(true);
		expect(selection.active).toBe(false);
	});

	it("keeps the visual snapshot stable while streamed content changes", () => {
		const content = new MutableComponent(["first response"]);
		const layout = new ChatScrollLayout(content, new MutableComponent([]), () => 4);
		layout.render(30);
		layout.beginTextSelection(0, 0);
		layout.updateTextSelection(0, 4);

		content.lines = ["rewritten response"];
		const rendered = layout.render(30);

		expect(rendered[0]).toContain("first");
		expect(layout.finishTextSelection()).toBe("first");
	});

	it("keeps selecting across chat viewports while the visible area scrolls", () => {
		const content = new MutableComponent(["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]);
		const layout = new ChatScrollLayout(content, new MutableComponent([]), () => 4);
		layout.render(30);

		// The initial viewport contains rows 6–9. Start from its first row,
		// scroll upward, then extend the selection to the newly visible row 4.
		layout.beginTextSelection(0, 0);
		layout.scrollLines("up", 2);
		layout.render(30);
		layout.updateTextSelection(0, 0);

		expect(layout.finishTextSelection()).toBe("4\n5\n6");
	});
});
