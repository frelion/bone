import { extractAnsiCode, normalizeTerminalOutput, sliceByColumn, visibleWidth } from "@frelion/bone-tui";

export type ChatTextSelectionPoint = {
	row: number;
	column: number;
};

const graphemeSegmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });

function comparePoints(a: ChatTextSelectionPoint, b: ChatTextSelectionPoint): number {
	return a.row === b.row ? a.column - b.column : a.row - b.row;
}

function stripTerminalFormatting(line: string): string {
	const normalized = normalizeTerminalOutput(line);
	let text = "";
	let offset = 0;
	while (offset < normalized.length) {
		const ansi = extractAnsiCode(normalized, offset);
		if (ansi) {
			offset += ansi.length;
			continue;
		}
		text += normalized[offset]!;
		offset++;
	}
	return text;
}

/** Snap a mouse column to a complete displayed grapheme so wide glyphs are never split. */
function snapColumn(line: string, column: number, edge: "start" | "end"): number {
	const target = Math.max(0, Math.min(column, visibleWidth(line)));
	let currentColumn = 0;
	let offset = 0;

	while (offset < line.length) {
		const ansi = extractAnsiCode(line, offset);
		if (ansi) {
			offset += ansi.length;
			continue;
		}

		let textEnd = offset;
		while (textEnd < line.length && !extractAnsiCode(line, textEnd)) textEnd++;
		for (const { segment } of graphemeSegmenter.segment(line.slice(offset, textEnd))) {
			const nextColumn = currentColumn + visibleWidth(segment);
			if (target > currentColumn && target < nextColumn) {
				return edge === "start" ? currentColumn : nextColumn;
			}
			if (target === currentColumn || target === nextColumn) return target;
			currentColumn = nextColumn;
		}
		offset = textEnd;
	}

	return target;
}

/**
 * Holds a stable visual chat snapshot while a mouse drag is in progress.
 *
 * The snapshot prevents streamed output from shifting the selected rows or
 * columns between mouse down and mouse up. It also centralizes ANSI-safe
 * rendering and clipboard text extraction so other TUI panes can reuse the
 * same selection semantics later.
 */
export class ChatTextSelection {
	private snapshot: string[] | undefined;
	private anchor: ChatTextSelectionPoint | undefined;
	private focus: ChatTextSelectionPoint | undefined;

	get active(): boolean {
		return this.snapshot !== undefined;
	}

	get hasVisibleSelection(): boolean {
		return this.anchor !== undefined && this.focus !== undefined && comparePoints(this.anchor, this.focus) !== 0;
	}

	getSnapshot(): readonly string[] | undefined {
		return this.snapshot;
	}

	begin(lines: readonly string[], row: number, column: number): boolean {
		if (lines.length === 0) return false;
		this.snapshot = [...lines];
		const point = this.toPoint(row, column);
		this.anchor = point;
		this.focus = point;
		return true;
	}

	update(row: number, column: number): boolean {
		if (!this.snapshot || !this.anchor) return false;
		this.focus = this.toPoint(row, column);
		return true;
	}

	cancel(): boolean {
		if (!this.active) return false;
		this.snapshot = undefined;
		this.anchor = undefined;
		this.focus = undefined;
		return true;
	}

	copyText(): string | undefined {
		if (!this.snapshot || !this.anchor || !this.focus || !this.hasVisibleSelection) return undefined;
		const [start, end] =
			comparePoints(this.anchor, this.focus) <= 0 ? [this.anchor, this.focus] : [this.focus, this.anchor];
		const selectedLines: string[] = [];

		for (let row = start.row; row <= end.row; row++) {
			const line = this.snapshot[row] ?? "";
			const lineWidth = visibleWidth(line);
			const startColumn = row === start.row ? snapColumn(line, start.column, "start") : 0;
			const endColumn = row === end.row ? snapColumn(line, Math.min(lineWidth, end.column + 1), "end") : lineWidth;
			selectedLines.push(
				stripTerminalFormatting(sliceByColumn(line, startColumn, Math.max(0, endColumn - startColumn))),
			);
		}

		return selectedLines.join("\n");
	}

	renderLine(line: string, row: number): string {
		if (!this.snapshot || !this.anchor || !this.focus || !this.hasVisibleSelection) return line;
		const [start, end] =
			comparePoints(this.anchor, this.focus) <= 0 ? [this.anchor, this.focus] : [this.focus, this.anchor];
		if (row < start.row || row > end.row) return line;

		const lineWidth = visibleWidth(line);
		const startColumn = row === start.row ? snapColumn(line, start.column, "start") : 0;
		const endColumn = row === end.row ? snapColumn(line, Math.min(lineWidth, end.column + 1), "end") : lineWidth;
		if (startColumn >= endColumn) return line;

		const before = sliceByColumn(line, 0, startColumn);
		const selected = sliceByColumn(line, startColumn, endColumn - startColumn);
		const after = sliceByColumn(line, endColumn, Math.max(0, lineWidth - endColumn));
		// Reverse video works across all custom themes without adding a new theme token.
		return `${before}\x1b[7m${selected}\x1b[27m${after}`;
	}

	private toPoint(row: number, column: number): ChatTextSelectionPoint {
		const snapshot = this.snapshot;
		if (!snapshot) return { row: 0, column: 0 };
		const normalizedRow = Math.max(0, Math.min(Math.floor(row), snapshot.length - 1));
		const line = snapshot[normalizedRow] ?? "";
		const lineWidth = visibleWidth(line);
		return { row: normalizedRow, column: Math.max(0, Math.min(Math.floor(column), lineWidth)) };
	}
}
