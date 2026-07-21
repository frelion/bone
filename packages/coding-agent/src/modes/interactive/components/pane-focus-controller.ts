import type { Component, TUI } from "@frelion/bone-tui";

/** Coordinates focus between independently interactive panes. */
export class PaneFocusController {
	private readonly panes = new Map<string, Component>();
	private readonly ui: TUI;
	private readonly onFocused?: (id: string) => void;

	constructor(ui: TUI, onFocused?: (id: string) => void) {
		this.ui = ui;
		this.onFocused = onFocused;
	}

	register(id: string, component: Component): void {
		this.panes.set(id, component);
	}

	focus(id: string): boolean {
		const component = this.panes.get(id);
		if (!component) return false;
		this.ui.setFocus(component);
		this.onFocused?.(id);
		this.ui.requestRender();
		return true;
	}
}
