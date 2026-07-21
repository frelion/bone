import type { Component } from "@frelion/bone-tui";
import { truncateToWidth, visibleWidth } from "@frelion/bone-tui";

/** Renders two vertical component trees next to each other. */
export class SplitPane implements Component {
	private readonly sidebar: Component;
	private readonly main: Component;
	private readonly sidebarWidth: number;
	private readonly separator: string;
	private readonly minimumMainWidth: number;
	private readonly getViewportRows: () => number;

	constructor(
		sidebar: Component,
		main: Component,
		sidebarWidth: number = 30,
		separator: string = " │ ",
		minimumMainWidth: number = 44,
		getViewportRows: () => number = () => Number.POSITIVE_INFINITY,
	) {
		this.sidebar = sidebar;
		this.main = main;
		this.sidebarWidth = sidebarWidth;
		this.separator = separator;
		this.minimumMainWidth = minimumMainWidth;
		this.getViewportRows = getViewportRows;
	}

	invalidate(): void {
		this.sidebar.invalidate();
		this.main.invalidate();
	}

	render(width: number): string[] {
		const separatorWidth = visibleWidth(this.separator);
		if (width < this.sidebarWidth + separatorWidth + this.minimumMainWidth) {
			return this.main.render(width);
		}

		const mainWidth = width - this.sidebarWidth - separatorWidth;
		const viewportRows = Math.max(1, this.getViewportRows());
		const viewportAwareSidebar = this.sidebar as Component & { setViewportRows?: (rows: number) => void };
		viewportAwareSidebar.setViewportRows?.(viewportRows);
		const sidebarLines = this.sidebar.render(this.sidebarWidth);
		const mainLines = this.main.render(mainWidth);
		// Anchor the Side to the active terminal viewport rather than the start
		// of the scrollback buffer, so long chats never push it out of view.
		const sidebarStart = Math.max(0, mainLines.length - viewportRows);
		const height = Math.max(mainLines.length, sidebarStart + sidebarLines.length);
		const lines: string[] = [];

		for (let index = 0; index < height; index++) {
			const sidebarIndex = index - sidebarStart;
			const sidebar = truncateToWidth(sidebarLines[sidebarIndex] ?? "", this.sidebarWidth, "");
			const main = truncateToWidth(mainLines[index] ?? "", mainWidth, "");
			lines.push(
				`${sidebar}${" ".repeat(Math.max(0, this.sidebarWidth - visibleWidth(sidebar)))}${this.separator}${main}`,
			);
		}

		return lines;
	}
}
