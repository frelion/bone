import type { Component } from "@earendil-works/pi-tui";

/** Keeps chat history scrollable while the composer and status area stay visible. */
export class ChatScrollLayout implements Component {
	private readonly content: Component;
	private readonly fixedBottom: Component;
	private readonly getViewportRows: () => number;
	private scrollOffset = 0;
	private contentRows = 1;
	private previousContentLineCount: number | undefined;

	constructor(content: Component, fixedBottom: Component, getViewportRows: () => number) {
		this.content = content;
		this.fixedBottom = fixedBottom;
		this.getViewportRows = getViewportRows;
	}

	invalidate(): void {
		this.content.invalidate();
		this.fixedBottom.invalidate();
	}

	scrollPage(direction: "up" | "down"): boolean {
		const previousOffset = this.scrollOffset;
		const pageSize = Math.max(1, this.contentRows - 1);
		this.scrollOffset = Math.max(0, this.scrollOffset + (direction === "up" ? pageSize : -pageSize));
		return this.scrollOffset !== previousOffset;
	}

	getScrollOffset(): number {
		return this.scrollOffset;
	}

	/**
	 * Restore a previously saved conversation position. The next render must not
	 * mistake that conversation's existing history for newly streamed content.
	 */
	setScrollOffset(offset: number): void {
		this.scrollOffset = Number.isFinite(offset) ? Math.max(0, Math.floor(offset)) : 0;
		this.previousContentLineCount = undefined;
	}

	render(width: number): string[] {
		const viewportRows = Math.max(1, this.getViewportRows());
		const allFixedLines = this.fixedBottom.render(width);
		const fixedLines = allFixedLines.slice(-Math.max(0, viewportRows - 1));
		this.contentRows = Math.max(1, viewportRows - fixedLines.length);

		const contentLines = this.content.render(width);
		if (
			this.previousContentLineCount !== undefined &&
			this.scrollOffset > 0 &&
			contentLines.length > this.previousContentLineCount
		) {
			this.scrollOffset += contentLines.length - this.previousContentLineCount;
		}
		this.previousContentLineCount = contentLines.length;

		const maxOffset = Math.max(0, contentLines.length - this.contentRows);
		this.scrollOffset = Math.min(this.scrollOffset, maxOffset);
		const start = Math.max(0, contentLines.length - this.contentRows - this.scrollOffset);
		const visibleContent = contentLines.slice(start, start + this.contentRows);

		while (visibleContent.length < this.contentRows) {
			visibleContent.push("");
		}
		return [...visibleContent, ...fixedLines];
	}
}
