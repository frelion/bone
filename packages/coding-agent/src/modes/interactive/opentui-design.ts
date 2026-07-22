export type OpenTUILayoutMode = "split" | "single";

export const OPEN_TUI_LAYOUT = {
	compactBreakpoint: 88,
	sidebarMinWidth: 28,
	sidebarMaxWidth: 34,
	contentMaxWidth: 112,
	dialogMaxWidth: 92,
	dialogMargin: 2,
} as const;

export function resolveOpenTUILayoutMode(width: number): OpenTUILayoutMode {
	return width < OPEN_TUI_LAYOUT.compactBreakpoint ? "single" : "split";
}

export function resolveOpenTUISidebarWidth(width: number): number {
	return Math.min(
		OPEN_TUI_LAYOUT.sidebarMaxWidth,
		Math.max(OPEN_TUI_LAYOUT.sidebarMinWidth, Math.floor(width * 0.28)),
	);
}

export interface OpenTUIDialogLayout {
	width: number | "100%";
	height: number | "100%";
	maxHeight: number | "100%";
	margin: number;
}

export function resolveOpenTUIDialogLayout(width: number, height: number): OpenTUIDialogLayout {
	if (width < OPEN_TUI_LAYOUT.compactBreakpoint) {
		return { width: "100%", height: "100%", maxHeight: "100%", margin: 0 };
	}
	const availableWidth = Math.max(1, width - OPEN_TUI_LAYOUT.dialogMargin * 2);
	return {
		width: Math.min(OPEN_TUI_LAYOUT.dialogMaxWidth, availableWidth),
		height: Math.min(24, Math.max(8, height - OPEN_TUI_LAYOUT.dialogMargin * 2)),
		maxHeight: Math.max(8, height - OPEN_TUI_LAYOUT.dialogMargin * 2),
		margin: OPEN_TUI_LAYOUT.dialogMargin,
	};
}
