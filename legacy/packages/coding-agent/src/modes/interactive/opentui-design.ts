export type OpenTUILayoutMode = "split" | "single";

export const OPEN_TUI_LAYOUT = {
	sidebarDefaultWidth: 38,
	sidebarMinWidth: 32,
	sidebarMaxWidth: 60,
	mainMinWidth: 60,
	separatorWidth: 1,
	dialogMaxWidth: 92,
	dialogMargin: 2,
} as const;

export const OPEN_TUI_COLORS = {
	page: "#0a0a0a",
	panel: "#141414",
	element: "#1e1e1e",
	elementRaised: "#282828",
	border: "#484848",
	borderActive: "#606060",
	borderSubtle: "#3c3c3c",
	text: "#eeeeee",
	muted: "#808080",
	dim: "#606060",
	primary: "#fab283",
	primaryStrong: "#f5a742",
	primaryText: "#0a0a0a",
	selection: "#40291d",
	selectionText: "#ffc09f",
	success: "#7fd88f",
	error: "#e06c75",
	warning: "#e5c07b",
	info: "#56b6c2",
} as const;

export function clampOpenTUISidebarWidth(width: number): number {
	if (!Number.isFinite(width)) return OPEN_TUI_LAYOUT.sidebarDefaultWidth;
	return Math.min(OPEN_TUI_LAYOUT.sidebarMaxWidth, Math.max(OPEN_TUI_LAYOUT.sidebarMinWidth, Math.round(width)));
}

export function resolveOpenTUILayoutMode(
	width: number,
	sidebarWidth: number = OPEN_TUI_LAYOUT.sidebarDefaultWidth,
): OpenTUILayoutMode {
	const splitMinWidth =
		clampOpenTUISidebarWidth(sidebarWidth) + OPEN_TUI_LAYOUT.separatorWidth + OPEN_TUI_LAYOUT.mainMinWidth;
	return width >= splitMinWidth ? "split" : "single";
}

export interface OpenTUIDialogLayout {
	width: number | "100%";
	height: number | "100%";
	maxHeight: number | "100%";
	margin: number;
}

export function resolveOpenTUIDialogLayout(width: number, height: number): OpenTUIDialogLayout {
	if (resolveOpenTUILayoutMode(width) === "single") {
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
