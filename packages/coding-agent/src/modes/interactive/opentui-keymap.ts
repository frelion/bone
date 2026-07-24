import type { KeyEvent } from "@opentui/core";

export type OpenTUIAction =
	| "confirm"
	| "cancel"
	| "up"
	| "down"
	| "pageUp"
	| "pageDown"
	| "focusLeft"
	| "focusRight"
	| "focusDown"
	| "clear"
	| "exit"
	| "sidebarSearch"
	| "sidebarDelete"
	| "composerSubmit"
	| "composerQueue"
	| "composerNewline"
	| "composerNewlineAlt"
	| "composerHistoryUp"
	| "composerHistoryDown"
	| "composerAutocomplete"
	| "composerCancel"
	| "startupScope"
	| "startupExit"
	| "sessionSort"
	| "sessionNamedFilter"
	| "sessionPath"
	| "sessionDelete"
	| "configToggle"
	| "toggleToolDetails"
	| "jumpToLatest"
	| "save";

interface OpenTUIKeySpec {
	name: string;
	ctrl?: boolean;
	shift?: boolean;
	meta?: boolean;
}

const OPEN_TUI_KEYMAP: Record<OpenTUIAction, OpenTUIKeySpec> = {
	confirm: { name: "enter" },
	cancel: { name: "escape" },
	up: { name: "up" },
	down: { name: "down" },
	pageUp: { name: "pageup" },
	pageDown: { name: "pagedown" },
	focusLeft: { name: "left", shift: true },
	focusRight: { name: "right", shift: true },
	focusDown: { name: "down", shift: true },
	clear: { name: "c", ctrl: true },
	exit: { name: "d", ctrl: true },
	sidebarSearch: { name: "/" },
	sidebarDelete: { name: "d" },
	composerSubmit: { name: "enter" },
	composerQueue: { name: "enter", meta: true },
	composerNewline: { name: "enter", shift: true },
	composerNewlineAlt: { name: "j", ctrl: true },
	composerHistoryUp: { name: "up" },
	composerHistoryDown: { name: "down" },
	composerAutocomplete: { name: "tab" },
	composerCancel: { name: "escape" },
	startupScope: { name: "tab" },
	startupExit: { name: "c", ctrl: true },
	sessionSort: { name: "s", ctrl: true },
	sessionNamedFilter: { name: "n", ctrl: true },
	sessionPath: { name: "p", ctrl: true },
	sessionDelete: { name: "d", ctrl: true },
	configToggle: { name: "space" },
	toggleToolDetails: { name: "o", ctrl: true },
	jumpToLatest: { name: "end" },
	save: { name: "s", ctrl: true },
};

function normalizeName(name: string): string {
	const normalized = name.toLowerCase().replaceAll("-", "");
	if (normalized === "return") return "enter";
	if (normalized === "esc") return "escape";
	return normalized;
}

/** Match a product action against OpenTUI's structured keyboard event. */
export function matchesOpenTUIAction(event: KeyEvent, action: OpenTUIAction): boolean {
	if (event.eventType === "release") return false;
	const spec = OPEN_TUI_KEYMAP[action];
	return (
		normalizeName(event.name) === spec.name &&
		event.ctrl === (spec.ctrl ?? false) &&
		event.shift === (spec.shift ?? false) &&
		(event.meta || event.option) === (spec.meta ?? false)
	);
}
