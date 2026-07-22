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
	public onFocusSidebar?: () => void;
	public onFocusComposer?: () => void;
	public onNearOldestContent?: () => void;
	public onInterrupt?: () => void;
	public onExit?: () => void;

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

	private scroll(delta: number): void {
		this.transcript.scrollBy(delta);
		if (delta < 0 && this.transcript.scrollTop <= Math.max(2, this.getPageRows())) this.onNearOldestContent?.();
	}
}
