import type { ChatScrollLayout } from "./chat-scroll-layout.ts";

type InputListenerResult = { consume?: boolean } | undefined;

type SgrMouseEvent = {
	button: number;
	column: number;
	row: number;
	isRelease: boolean;
	isMotion: boolean;
	isWheel: boolean;
};

export type ChatTextSelectionBounds = {
	left: number;
	top: number;
	width: number;
	height: number;
};

type ChatTextSelectionControllerOptions = {
	layout: ChatScrollLayout;
	getBounds: () => ChatTextSelectionBounds;
	isBlocked: () => boolean;
	onRender: () => void;
	onCopy: (text: string) => Promise<void>;
	onCopied: (characterCount: number) => void;
	onCopyError: (error: unknown) => void;
};

function parseSgrMouseEvent(data: string): SgrMouseEvent | undefined {
	const match = data.match(/^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/);
	if (!match) return undefined;

	const button = Number.parseInt(match[1]!, 10);
	const column = Number.parseInt(match[2]!, 10);
	const row = Number.parseInt(match[3]!, 10);
	if (!Number.isSafeInteger(button) || !Number.isSafeInteger(column) || !Number.isSafeInteger(row)) return undefined;

	return {
		button,
		column,
		row,
		isRelease: match[4] === "m",
		isMotion: (button & 32) !== 0,
		isWheel: (button & 64) !== 0,
	};
}

/**
 * Routes SGR mouse drags to the chat selection surface while leaving wheel
 * events untouched for conversation scrolling. Mouse reporting is required
 * for wheel support, so native terminal selection cannot coexist reliably.
 */
export class ChatTextSelectionController {
	private readonly layout: ChatScrollLayout;
	private readonly getBounds: () => ChatTextSelectionBounds;
	private readonly isBlocked: () => boolean;
	private readonly onRender: () => void;
	private readonly onCopy: (text: string) => Promise<void>;
	private readonly onCopied: (characterCount: number) => void;
	private readonly onCopyError: (error: unknown) => void;

	constructor(options: ChatTextSelectionControllerOptions) {
		this.layout = options.layout;
		this.getBounds = options.getBounds;
		this.isBlocked = options.isBlocked;
		this.onRender = options.onRender;
		this.onCopy = options.onCopy;
		this.onCopied = options.onCopied;
		this.onCopyError = options.onCopyError;
	}

	handleInput(data: string): InputListenerResult {
		if (data === "\x1b" && this.layout.cancelTextSelection()) {
			this.onRender();
			return { consume: true };
		}

		const event = parseSgrMouseEvent(data);
		if (!event) return undefined;
		if (event.isWheel) return undefined;

		if (this.isBlocked()) {
			if (this.layout.cancelTextSelection()) this.onRender();
			return { consume: true };
		}

		const leftButton = (event.button & 3) === 0;
		if (!event.isRelease && !event.isMotion && leftButton) {
			const point = this.toInitialPoint(event);
			if (point) {
				this.layout.beginTextSelection(point.row, point.column);
				this.onRender();
			}
			return { consume: true };
		}

		if (event.isMotion && this.layout.textSelection.active) {
			const point = this.toDraggedPoint(event);
			if (point) {
				this.layout.updateTextSelection(point.row, point.column);
				this.onRender();
			}
			return { consume: true };
		}

		if (event.isRelease && this.layout.textSelection.active) {
			const point = this.toDraggedPoint(event);
			if (point) this.layout.updateTextSelection(point.row, point.column);
			const text = this.layout.finishTextSelection();
			this.onRender();
			if (text) void this.copy(text);
			return { consume: true };
		}

		return { consume: true };
	}

	cancel(): void {
		if (this.layout.cancelTextSelection()) this.onRender();
	}

	private toInitialPoint(event: SgrMouseEvent): { row: number; column: number } | undefined {
		const bounds = this.getBounds();
		const x = event.column - 1;
		const y = event.row - 1;
		if (bounds.width <= 0 || bounds.height <= 0) return undefined;
		if (x < bounds.left || x >= bounds.left + bounds.width || y < bounds.top || y >= bounds.top + bounds.height) {
			return undefined;
		}
		return { row: y - bounds.top, column: x - bounds.left };
	}

	private toDraggedPoint(event: SgrMouseEvent): { row: number; column: number } | undefined {
		const bounds = this.getBounds();
		if (bounds.width <= 0 || bounds.height <= 0) return undefined;
		const x = event.column - 1;
		const y = event.row - 1;
		return {
			row: Math.max(0, Math.min(y - bounds.top, bounds.height - 1)),
			column: Math.max(0, Math.min(x - bounds.left, bounds.width - 1)),
		};
	}

	private async copy(text: string): Promise<void> {
		try {
			await this.onCopy(text);
			this.onCopied(Array.from(text).length);
		} catch (error) {
			this.onCopyError(error);
		}
	}
}

export { parseSgrMouseEvent };
