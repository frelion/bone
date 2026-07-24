import type { ScrollBoxRenderable } from "@opentui/core";

export type OpenTUITranscriptUpdateKind = "content" | "tool" | "completion";

export interface OpenTUITranscriptFocusState {
	following: boolean;
	unseenUpdateCount: number;
	latestUpdateKind: OpenTUITranscriptUpdateKind | undefined;
}

/** Product-level auto-follow policy for the native transcript viewport. */
export class OpenTUITranscriptFocusController {
	private readonly transcript: ScrollBoxRenderable;
	private readonly getPageRows: () => number;
	private autoFollowing = true;
	private unseenUpdateCount = 0;
	private latestUpdateKind: OpenTUITranscriptUpdateKind | undefined;
	public onNearOldestContent?: () => void;
	public onAutoFollowChange?: (following: boolean) => void;
	public onStateChange?: (state: OpenTUITranscriptFocusState) => void;

	constructor(transcript: ScrollBoxRenderable, getPageRows: () => number) {
		this.transcript = transcript;
		this.getPageRows = getPageRows;
	}

	isAutoFollowing(): boolean {
		return this.autoFollowing;
	}

	getState(): OpenTUITranscriptFocusState {
		return {
			following: this.autoFollowing,
			unseenUpdateCount: this.unseenUpdateCount,
			latestUpdateKind: this.latestUpdateKind,
		};
	}

	restoreState(state: OpenTUITranscriptFocusState): void {
		this.autoFollowing = state.following;
		this.unseenUpdateCount = Math.max(0, Math.floor(state.unseenUpdateCount));
		this.latestUpdateKind = state.latestUpdateKind;
		this.emitStateChange();
	}

	/** Record one user-meaningful transcript update while the viewport is paused.
	 * Callers should invoke this for semantic events, not individual stream deltas. */
	recordSemanticUpdate(kind: OpenTUITranscriptUpdateKind = "content"): void {
		if (this.autoFollowing) return;
		this.unseenUpdateCount += 1;
		this.latestUpdateKind = kind;
		this.emitStateChange();
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
			const following = predicted >= bottom - 1;
			this.setAutoFollowing(following, following);
		}
	}

	followLatest(): void {
		this.transcript.scrollTo(Number.MAX_SAFE_INTEGER);
		this.setAutoFollowing(true, true);
	}

	jumpToLatest(): void {
		this.followLatest();
	}

	syncAutoFollow(): void {
		const bottom = Math.max(0, this.transcript.scrollHeight - this.pageRows);
		const following = this.transcript.scrollTop >= bottom - 1;
		this.setAutoFollowing(following, following);
	}

	private get pageRows(): number {
		return Math.max(1, this.transcript.viewport.height || this.getPageRows());
	}

	private setAutoFollowing(following: boolean, clearUnseen = false): void {
		const followingChanged = following !== this.autoFollowing;
		const unseenChanged = clearUnseen && (this.unseenUpdateCount > 0 || this.latestUpdateKind !== undefined);
		if (!followingChanged && !unseenChanged) return;
		this.autoFollowing = following;
		if (clearUnseen) {
			this.unseenUpdateCount = 0;
			this.latestUpdateKind = undefined;
		}
		if (followingChanged) this.onAutoFollowChange?.(following);
		this.emitStateChange();
	}

	private emitStateChange(): void {
		this.onStateChange?.(this.getState());
	}
}
