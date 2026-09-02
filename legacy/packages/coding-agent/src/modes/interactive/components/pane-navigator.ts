import { CliRenderEvents, type CliRenderer, type Renderable } from "@opentui/core";

export interface OpenTUIPaneRegistration {
	readonly root: Renderable;
	readonly focusTarget: Renderable | (() => Renderable);
	onFocusChange?: (focused: boolean) => void;
}

/** Maps product panes to attached native focus targets without routing control input. */
export class OpenTUIPaneNavigator {
	private readonly panes = new Map<string, OpenTUIPaneRegistration>();
	private readonly renderer: CliRenderer;
	private readonly onFocused: ((id: string) => void) | undefined;
	private readonly focusHandler: (current: Renderable | null) => void;
	private activeId: string | undefined;

	constructor(renderer: CliRenderer, onFocused?: (id: string) => void) {
		this.renderer = renderer;
		this.onFocused = onFocused;
		this.focusHandler = (current) => {
			const paneId = this.findPaneId(current);
			if (paneId) this.setActive(paneId);
		};
		renderer.on(CliRenderEvents.FOCUSED_RENDERABLE, this.focusHandler);
	}

	get focusedPane(): string | undefined {
		return this.activeId;
	}

	get focusTarget(): Renderable | null {
		const target = this.activeId ? this.panes.get(this.activeId)?.focusTarget : undefined;
		return target ? (typeof target === "function" ? target() : target) : null;
	}

	register(id: string, pane: OpenTUIPaneRegistration): void {
		this.panes.set(id, pane);
		if (this.isDescendantOrSelf(this.renderer.currentFocusedRenderable, pane.root)) this.setActive(id);
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
		const target = pane
			? typeof pane.focusTarget === "function"
				? pane.focusTarget()
				: pane.focusTarget
			: undefined;
		if (!pane || !target || !this.isAttached(target) || !this.isEffectivelyVisible(target)) return false;
		this.setActive(id);
		target.focus();
		return this.renderer.currentFocusedRenderable === target;
	}

	dispose(): void {
		this.renderer.off(CliRenderEvents.FOCUSED_RENDERABLE, this.focusHandler);
		if (this.activeId) this.panes.get(this.activeId)?.onFocusChange?.(false);
		this.activeId = undefined;
		this.panes.clear();
	}

	private setActive(id: string): void {
		if (this.activeId === id) return;
		if (this.activeId) this.panes.get(this.activeId)?.onFocusChange?.(false);
		this.activeId = id;
		this.panes.get(id)?.onFocusChange?.(true);
		this.onFocused?.(id);
		this.renderer.requestRender();
	}

	private findPaneId(current: Renderable | null): string | undefined {
		for (const [id, pane] of this.panes) {
			if (this.isDescendantOrSelf(current, pane.root)) return id;
		}
		return undefined;
	}

	private isAttached(node: Renderable): boolean {
		let current: Renderable | null = node;
		while (current) {
			if (current === this.renderer.root) return true;
			current = current.parent;
		}
		return false;
	}

	private isEffectivelyVisible(node: Renderable): boolean {
		let current: Renderable | null = node;
		while (current) {
			if (current.isDestroyed || !current.visible) return false;
			current = current.parent;
		}
		return true;
	}

	private isDescendantOrSelf(node: Renderable | null, ancestor: Renderable): boolean {
		let current = node;
		while (current) {
			if (current === ancestor) return true;
			current = current.parent;
		}
		return false;
	}
}
