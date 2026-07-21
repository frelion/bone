import type { Component } from "@frelion/bone-tui";
import { ChatTextSelection } from "./chat-text-selection.ts";

/** Keeps chat history scrollable while the composer and status area stay visible. */
export class ChatScrollLayout implements Component {
	private readonly content: Component;
	private readonly fixedBottom: Component;
	private readonly getViewportRows: () => number;
	private scrollOffset = 0;
	private contentRows = 1;
	private previousContentLineCount: number | undefined;
	private contentLines: string[] = [];
	private visibleContentLines: string[] = [];
	private visibleContentStart = 0;
	readonly textSelection = new ChatTextSelection();

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
		const pageSize = Math.max(1, this.contentRows - 1);
		return this.scrollLines(direction, pageSize);
	}

	/** Scroll by a small, bounded number of rendered chat rows. */
	scrollLines(direction: "up" | "down", lineCount = 1): boolean {
		const previousOffset = this.scrollOffset;
		const amount = Math.max(1, Math.floor(lineCount));
		const selectionSnapshot = this.textSelection.getSnapshot();
		const sourceLines = selectionSnapshot ?? this.contentLines;
		const maxOffset = Math.max(0, sourceLines.length - this.contentRows);
		this.scrollOffset = Math.max(0, Math.min(maxOffset, this.scrollOffset + (direction === "up" ? amount : -amount)));
		return this.scrollOffset !== previousOffset;
	}

	getScrollOffset(): number {
		return this.scrollOffset;
	}

	getVisibleContentRowCount(): number {
		return this.visibleContentLines.length;
	}

	/** Whether upward navigation is close enough to the loaded boundary to prefetch older history. */
	isNearOldestContent(): boolean {
		const maxOffset = Math.max(0, this.contentLines.length - this.contentRows);
		return maxOffset - this.scrollOffset <= Math.max(1, this.contentRows * 2);
	}

	beginTextSelection(row: number, column: number): boolean {
		return this.textSelection.begin(this.contentLines, this.visibleContentStart + row, column);
	}

	updateTextSelection(row: number, column: number): boolean {
		return this.textSelection.update(this.visibleContentStart + row, column);
	}

	finishTextSelection(): string | undefined {
		const text = this.textSelection.copyText();
		this.textSelection.cancel();
		return text;
	}

	cancelTextSelection(): boolean {
		return this.textSelection.cancel();
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

		const liveContentLines = this.content.render(width);
		this.contentLines = liveContentLines;
		const selectionSnapshot = this.textSelection.getSnapshot();
		if (
			!selectionSnapshot &&
			this.previousContentLineCount !== undefined &&
			this.scrollOffset > 0 &&
			liveContentLines.length > this.previousContentLineCount
		) {
			this.scrollOffset += liveContentLines.length - this.previousContentLineCount;
		}
		if (!selectionSnapshot) this.previousContentLineCount = liveContentLines.length;

		const sourceLines = selectionSnapshot ?? liveContentLines;
		const maxOffset = Math.max(0, sourceLines.length - this.contentRows);
		this.scrollOffset = Math.min(this.scrollOffset, maxOffset);
		const start = Math.max(0, sourceLines.length - this.contentRows - this.scrollOffset);
		const visibleContent = sourceLines.slice(start, start + this.contentRows);

		while (visibleContent.length < this.contentRows) {
			visibleContent.push("");
		}

		this.visibleContentStart = start;
		this.visibleContentLines = [...visibleContent];
		const renderedContent = visibleContent.map((line, row) => this.textSelection.renderLine(line, start + row));
		return [...renderedContent, ...fixedLines];
	}
}
