import { type Component, visibleWidth } from "@frelion/bone-tui";
import { type ThemeColor, theme } from "../theme/theme.ts";
import { keyText } from "./keybinding-hints.ts";
import { fitLine } from "./terminal-layout.ts";

export type WorkspaceStatusTone = "success" | "warning" | "error" | "muted" | "accent";

export interface WorkspaceStatusTraySnapshot {
	search: {
		label: string;
		detail: string;
		tone: WorkspaceStatusTone;
	};
	sessions: {
		current: string;
		background: string;
		stored: number;
	};
	runtime: {
		label: string;
		detail?: string;
	};
}

function toneColor(tone: WorkspaceStatusTone): ThemeColor {
	return tone === "muted" ? "muted" : tone;
}

function renderHeading(width: number): string {
	const title = ` Workspace status · ${keyText("app.interrupt")} close `;
	const availableRuleWidth = Math.max(0, width - visibleWidth(title));
	return fitLine(
		`${theme.fg("borderMuted", "─".repeat(availableRuleWidth))}${theme.bold(theme.fg("text", title))}`,
		width,
	);
}

function renderSection(label: string, value: string, width: number): string {
	const prefix = theme.fg("muted", `${label.padEnd(16)} `);
	return fitLine(`${prefix}${value}`, width, "…");
}

/**
 * A read-only, non-focusable workspace status region below the composer.
 * InteractiveMode owns the data refresh and lifecycle; this component only
 * renders the current snapshot within its assigned terminal width.
 */
export class WorkspaceStatusTray implements Component {
	private _visible = false;
	private snapshot: WorkspaceStatusTraySnapshot = {
		search: { label: "Loading", detail: "Reading workspace status", tone: "accent" },
		sessions: { current: "loading", background: "loading", stored: 0 },
		runtime: { label: "Loading local runtime" },
	};

	get visible(): boolean {
		return this._visible;
	}

	setVisible(visible: boolean): void {
		this._visible = visible;
	}

	setSnapshot(snapshot: WorkspaceStatusTraySnapshot): void {
		this.snapshot = snapshot;
	}

	invalidate(): void {}

	render(width: number): string[] {
		if (!this._visible || width <= 0) return [];

		const searchSummary = `${theme.fg(toneColor(this.snapshot.search.tone), "●")} ${theme.fg(
			toneColor(this.snapshot.search.tone),
			this.snapshot.search.label,
		)}${theme.fg("muted", ` · ${this.snapshot.search.detail}`)}`;
		const compactSummary = `Search ${this.snapshot.search.label.toLowerCase()} · ${this.snapshot.search.detail.toLowerCase()}`;

		if (width < 62) {
			return [
				renderHeading(width),
				fitLine(theme.fg(toneColor(this.snapshot.search.tone), `● ${compactSummary}`), width, "…"),
			];
		}

		const sessionSummary = `Current ${this.snapshot.sessions.current} · background ${this.snapshot.sessions.background} · stored ${this.snapshot.sessions.stored.toLocaleString()}`;
		const runtimeSummary = this.snapshot.runtime.detail
			? `${this.snapshot.runtime.label} · ${this.snapshot.runtime.detail}`
			: this.snapshot.runtime.label;
		return [
			renderHeading(width),
			renderSection("Search & memory", searchSummary, width),
			renderSection("Sessions", theme.fg("text", sessionSummary), width),
			renderSection("Runtime", theme.fg("text", runtimeSummary), width),
		];
	}
}
