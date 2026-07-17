import { type Component, type Focusable, getKeybindings } from "@earendil-works/pi-tui";

/**
 * Focus target for the read-only conversation transcript.
 *
 * It deliberately renders no rows: the surrounding ChatScrollLayout remains
 * responsible for layout. Giving transcript navigation its own focus target
 * means the app can use the same Shift+arrow focus contract as Side and the
 * composer without overloading editor cursor keys.
 */
export class ChatHistoryFocus implements Component, Focusable {
	focused = false;
	public onFocusSidebar?: () => void;
	public onFocusComposer?: () => void;
	public onScroll?: (direction: "up" | "down") => void;
	/** Keep application-level quit/interrupt behavior available while history owns focus. */
	public onInterrupt?: () => void;
	public onExit?: () => void;

	invalidate(): void {}

	render(): string[] {
		return [];
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (keybindings.matches(data, "app.clear")) {
			this.onInterrupt?.();
			return;
		}
		if (keybindings.matches(data, "app.exit")) {
			this.onExit?.();
			return;
		}
		if (keybindings.matches(data, "app.focus.left")) {
			this.onFocusSidebar?.();
			return;
		}
		if (keybindings.matches(data, "app.focus.down")) {
			this.onFocusComposer?.();
			return;
		}
		if (keybindings.matches(data, "tui.select.up") || keybindings.matches(data, "tui.select.pageUp")) {
			this.onScroll?.("up");
			return;
		}
		if (keybindings.matches(data, "tui.select.down") || keybindings.matches(data, "tui.select.pageDown")) {
			this.onScroll?.("down");
		}
	}
}
