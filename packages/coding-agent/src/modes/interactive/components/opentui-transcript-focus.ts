import type { ScrollBoxRenderable } from "@opentui/core";

/** Product-level auto-follow policy for the native transcript viewport. */
export class OpenTUITranscriptFocusController {
	private readonly transcript: ScrollBoxRenderable;
	private readonly getPageRows: () => number;
	private autoFollowing = true;
	public onNearOldestContent?: () => void;
	public onAutoFollowChange?: (following: boolean) => void;

	constructor(transcript: ScrollBoxRenderable, getPageRows: () => number) {
		this.transcript = transcript;
		this.getPageRows = getPageRows;
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

	/** Sync auto-follow against the position native ScrollBox will reach after
	 * its own mouse handler runs. This callback is invoked before OpenTUI's
	 * ScrollBox handler, so reading scrollTop directly would be one event late. */
	handleNativeMouseScroll(direction: "up" | "down", delta: number): void {
		const bottom = Math.max(0, this.transcript.scrollHeight - this.pageRows);
		const predicted = Math.max(
			0,
			Math.min(bottom, this.transcript.scrollTop + (direction === "up" ? -delta : delta)),
		);
		if (direction === "up") {
			this.setAutoFollowing(false);
			if (predicted <= Math.max(2, this.pageRows)) this.onNearOldestContent?.();
		} else {
			this.setAutoFollowing(predicted >= bottom - 1);
		}
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
		return Math.max(1, this.transcript.viewport.height || this.getPageRows());
	}

	private setAutoFollowing(following: boolean): void {
		if (following === this.autoFollowing) return;
		this.autoFollowing = following;
		this.onAutoFollowChange?.(following);
	}
}
