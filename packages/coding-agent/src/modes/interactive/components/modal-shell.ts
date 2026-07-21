import type { Component } from "@frelion/bone-tui";
import { visibleWidth } from "@frelion/bone-tui";
import { theme } from "../theme/theme.ts";
import { fitLine } from "./terminal-layout.ts";

export interface ModalShellOptions {
	title: () => string;
	renderHeader?: (width: number) => string[];
	renderBody: (width: number, height: number) => string[];
	renderFooter: (width: number) => string[];
}

/**
 * Fixed-frame modal for interactive overlays. Header and footer never enter
 * the scrollable body, and this is the only component that draws its border.
 */
export class ModalShell implements Component {
	private viewportRows: number | undefined;
	private readonly options: ModalShellOptions;

	constructor(options: ModalShellOptions) {
		this.options = options;
	}

	setViewportRows(rows: number): void {
		this.viewportRows = Math.max(7, rows);
	}

	invalidate(): void {}

	render(width: number): string[] {
		const frameWidth = Math.max(16, width);
		const innerWidth = Math.max(1, frameWidth - 2);
		const contentWidth = Math.max(1, innerWidth - 2);
		const header = this.options.renderHeader?.(contentWidth) ?? [];
		const footer = this.options.renderFooter(contentWidth);
		// Published pi-tui versions do not yet provide setViewportRows(). Keep
		// overlays safe when Bone is globally installed against one of those
		// versions by matching the settings overlay's 88% height policy locally.
		const maxRows = this.viewportRows ?? Math.max(7, Math.floor((process.stdout.rows || 24) * 0.88));
		const bodyHeight = Math.max(1, maxRows - 2 - header.length - footer.length);
		const body = this.options.renderBody(contentWidth, bodyHeight).slice(0, bodyHeight);
		const blankRows = Math.max(0, bodyHeight - body.length);
		const title = ` ${this.options.title()} `;
		const top = `┌${title}${"─".repeat(Math.max(0, innerWidth - visibleWidth(title)))}┐`;
		const wrap = (line = "") => `│ ${fitLine(line, contentWidth)} │`;

		return [
			theme.fg("borderAccent", fitLine(top, frameWidth)),
			...header.map(wrap),
			...body.map(wrap),
			...Array.from({ length: blankRows }, () => wrap()),
			...footer.map(wrap),
			theme.fg("borderAccent", `└${"─".repeat(innerWidth)}┘`),
		];
	}
}
