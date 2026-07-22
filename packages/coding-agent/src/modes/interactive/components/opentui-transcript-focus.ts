import type { BoneKeyEvent, BoneScrollViewNode } from "@frelion/bone-tui";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";
import type { OpenTUIPane } from "./pane-focus-controller.ts";

function consume(event: BoneKeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Product-level focus and keyboard scrolling for the structured transcript viewport. */
export class OpenTUITranscriptFocusController {
	private readonly transcript: BoneScrollViewNode;
	private readonly getPageRows: () => number;
	private autoFollowing = true;
	public onFocusSidebar?: () => void;
	public onFocusComposer?: () => void;
	public onNearOldestContent?: () => void;
	public onInterrupt?: () => void;
	public onExit?: () => void;
	public onAutoFollowChange?: (following: boolean) => void;

	constructor(transcript: BoneScrollViewNode, getPageRows: () => number) {
		this.transcript = transcript;
		this.getPageRows = getPageRows;
	}

	toPane(): OpenTUIPane {
		return {
			node: this.transcript,
			handleKey: (event) => this.handleKey(event),
		};
	}

	handleKey(event: BoneKeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (matchesOpenTUIAction(event, "clear")) {
			this.onInterrupt?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "exit")) {
			this.onExit?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "focusLeft")) {
			this.onFocusSidebar?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "focusDown")) {
			this.onFocusComposer?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "up")) {
			this.scroll(-1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "down")) {
			this.scroll(1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "pageUp")) {
			this.scroll(-Math.max(1, this.getPageRows() - 2));
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "pageDown")) {
			this.scroll(Math.max(1, this.getPageRows() - 2));
			return consume(event);
		}
		return false;
	}

	isAutoFollowing(): boolean {
		return this.autoFollowing;
	}

	scrollByUser(delta: number): void {
		this.transcript.scrollBy(delta);
		if (delta < 0) {
			this.setAutoFollowing(false);
			if (this.transcript.scrollTop <= Math.max(2, this.pageRows)) this.onNearOldestContent?.();
			return;
		}
		this.syncAutoFollow();
	}

	handleMouseScroll(delta: number): void {
		this.scrollByUser(delta);
	}

	followLatest(): void {
		this.transcript.scrollTo(Number.MAX_SAFE_INTEGER);
		this.setAutoFollowing(true);
	}

	syncAutoFollow(): void {
		const bottom = Math.max(0, this.transcript.scrollHeight - this.pageRows);
		this.setAutoFollowing(this.transcript.scrollTop >= bottom - 1);
	}

	private get pageRows(): number {
		return Math.max(1, this.transcript.viewportHeight || this.getPageRows());
	}

	private scroll(delta: number): void {
		this.scrollByUser(delta);
	}

	private setAutoFollowing(following: boolean): void {
		if (following === this.autoFollowing) return;
		this.autoFollowing = following;
		this.onAutoFollowChange?.(following);
	}
}
