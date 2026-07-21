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
	onSelectionStart?: () => void;
	onAutoScroll?: (direction: "up" | "down") => boolean;
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
	private static readonly AUTO_SCROLL_INTERVAL_MS = 40;
	private readonly layout: ChatScrollLayout;
	private readonly getBounds: () => ChatTextSelectionBounds;
	private readonly isBlocked: () => boolean;
	private readonly onRender: () => void;
	private readonly onSelectionStart: (() => void) | undefined;
	private readonly onAutoScroll: ((direction: "up" | "down") => boolean) | undefined;
	private readonly onCopy: (text: string) => Promise<void>;
	private readonly onCopied: (characterCount: number) => void;
	private readonly onCopyError: (error: unknown) => void;
	private autoScrollTimer: NodeJS.Timeout | undefined;
	private autoScrollDirection: "up" | "down" | undefined;
	private lastDragEvent: SgrMouseEvent | undefined;

	constructor(options: ChatTextSelectionControllerOptions) {
		this.layout = options.layout;
		this.getBounds = options.getBounds;
		this.isBlocked = options.isBlocked;
		this.onRender = options.onRender;
		this.onSelectionStart = options.onSelectionStart;
		this.onAutoScroll = options.onAutoScroll;
		this.onCopy = options.onCopy;
		this.onCopied = options.onCopied;
		this.onCopyError = options.onCopyError;
	}

	handleInput(data: string): InputListenerResult {
		if (data === "\x1b" && this.cancelSelection()) {
			this.onRender();
			return { consume: true };
		}

		const event = parseSgrMouseEvent(data);
		if (!event) return undefined;
		if (event.isWheel) return undefined;

		if (this.isBlocked()) {
			if (this.cancelSelection()) this.onRender();
			return { consume: true };
		}

		const leftButton = (event.button & 3) === 0;
		if (!event.isRelease && !event.isMotion && leftButton) {
			const point = this.toInitialPoint(event);
			if (point) {
				this.stopAutoScroll();
				this.onSelectionStart?.();
				this.layout.beginTextSelection(point.row, point.column);
				this.onRender();
			}
			return { consume: true };
		}

		if (event.isMotion && this.layout.textSelection.active) {
			this.lastDragEvent = event;
			const point = this.toDraggedPoint(event);
			if (point) {
				this.layout.updateTextSelection(point.row, point.column);
			}
			this.updateAutoScroll(event);
			this.onRender();
			return { consume: true };
		}

		if (event.isRelease && this.layout.textSelection.active) {
			this.stopAutoScroll();
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
		if (this.cancelSelection()) this.onRender();
	}

	private updateAutoScroll(event: SgrMouseEvent): void {
		const bounds = this.getBounds();
		const y = event.row - 1;
		// Terminals clamp mouse coordinates at the top edge to row 1, so a drag
		// cannot report a value above the screen. Treat the first chat row as the
		// upward auto-scroll edge zone; the lower edge can use the real area below
		// the scrollable chat because the fixed composer occupies it.
		const direction = y <= bounds.top ? "up" : y >= bounds.top + bounds.height ? "down" : undefined;
		if (!direction) {
			this.stopAutoScroll();
			return;
		}
		if (!this.onAutoScroll) return;
		if (this.autoScrollDirection === direction) return;
		this.stopAutoScroll();
		this.lastDragEvent = event;
		this.autoScrollDirection = direction;
		this.autoScrollSelection();
	}

	private autoScrollSelection(): void {
		const direction = this.autoScrollDirection;
		if (!direction || !this.layout.textSelection.active || !this.onAutoScroll) {
			this.stopAutoScroll();
			return;
		}
		if (!this.onAutoScroll(direction)) {
			this.stopAutoScroll();
			return;
		}
		const point = this.lastDragEvent ? this.toDraggedPoint(this.lastDragEvent) : undefined;
		if (point) this.layout.updateTextSelection(point.row, point.column);
		this.onRender();
		this.autoScrollTimer = setTimeout(() => {
			this.autoScrollTimer = undefined;
			this.autoScrollSelection();
		}, ChatTextSelectionController.AUTO_SCROLL_INTERVAL_MS);
	}

	private stopAutoScroll(): void {
		this.autoScrollDirection = undefined;
		this.lastDragEvent = undefined;
		if (this.autoScrollTimer) {
			clearTimeout(this.autoScrollTimer);
			this.autoScrollTimer = undefined;
		}
	}

	private cancelSelection(): boolean {
		this.stopAutoScroll();
		return this.layout.cancelTextSelection();
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
