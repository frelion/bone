import type { BoneKeyEvent, BoneNode, BoneRenderer, BoneUnsubscribe } from "@frelion/bone-tui";

export interface OpenTUIPane {
	node: BoneNode;
	handleKey?: (event: BoneKeyEvent) => boolean;
	onFocusChange?: (focused: boolean) => void;
}

/** Routes structured renderer input to the currently focused application pane. */
export class OpenTUIPaneFocusController {
	private readonly panes = new Map<string, OpenTUIPane>();
	private readonly renderer: BoneRenderer;
	private readonly onFocused?: (id: string) => void;
	private readonly unsubscribe: BoneUnsubscribe;
	private activeId: string | undefined;

	constructor(renderer: BoneRenderer, onFocused?: (id: string) => void) {
		this.renderer = renderer;
		this.onFocused = onFocused;
		this.unsubscribe = renderer.onKey((event) => {
			if (!this.activeId) return;
			this.panes.get(this.activeId)?.handleKey?.(event);
		});
	}

	get focusedPane(): string | undefined {
		return this.activeId;
	}

	register(id: string, pane: OpenTUIPane): void {
		this.panes.set(id, pane);
	}

	unregister(id: string): void {
		const pane = this.panes.get(id);
		if (!pane) return;
		if (this.activeId === id) {
			pane.onFocusChange?.(false);
			this.activeId = undefined;
		}
		this.panes.delete(id);
	}

	focus(id: string): boolean {
		const pane = this.panes.get(id);
		if (!pane) return false;
		if (this.activeId && this.activeId !== id) this.panes.get(this.activeId)?.onFocusChange?.(false);
		this.activeId = id;
		this.renderer.focus(pane.node);
		pane.onFocusChange?.(true);
		this.onFocused?.(id);
		this.renderer.requestRender();
		return true;
	}

	dispose(): void {
		if (this.activeId) this.panes.get(this.activeId)?.onFocusChange?.(false);
		this.activeId = undefined;
		this.unsubscribe();
		this.panes.clear();
	}
}
