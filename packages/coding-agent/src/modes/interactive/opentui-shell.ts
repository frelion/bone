import {
	BoxRenderable,
	CliRenderEvents,
	type CliRenderer,
	type MouseEvent,
	type Renderable,
	ScrollBoxRenderable,
} from "@opentui/core";
import {
	clampOpenTUISidebarWidth,
	OPEN_TUI_COLORS,
	OPEN_TUI_LAYOUT,
	type OpenTUILayoutMode,
	resolveOpenTUILayoutMode,
} from "./opentui-design.ts";
import type { Theme } from "./theme/theme.ts";

export interface OpenTUIInteractiveShellOptions {
	sidebarWidth?: number;
}

export type OpenTUIPrimaryPane = "sidebar" | "main";

/** Full-width native OpenTUI workspace with one transcript scroll owner. */
export class OpenTUIInteractiveShell {
	readonly root: BoxRenderable;
	readonly transcript: ScrollBoxRenderable;
	public onTranscriptScrollRequest?: (deltaRows: number) => void;
	public onTranscriptContentChange?: () => void;
	public onSidebarWidthChange?: (width: number) => void;
	private readonly renderer: CliRenderer;
	private readonly bodyRoot: BoxRenderable;
	private readonly sidebarRoot: BoxRenderable;
	private readonly separatorRoot: BoxRenderable;
	private readonly mainRoot: BoxRenderable;
	private readonly fixedRoot: BoxRenderable;
	private readonly headerRoot: BoxRenderable;
	private readonly aboveEditorRoot: BoxRenderable;
	private readonly editorRoot: BoxRenderable;
	private readonly belowEditorRoot: BoxRenderable;
	private readonly footerRoot: BoxRenderable;
	private readonly resizeHandler: () => void;
	private sidebarWidthValue: number;
	private activePane: OpenTUIPrimaryPane = "main";
	private layoutModeValue: OpenTUILayoutMode = "split";
	private separatorHovered = false;
	private separatorDragging = false;
	private sidebarPersistTimer: ReturnType<typeof setTimeout> | undefined;

	constructor(renderer: CliRenderer, options: OpenTUIInteractiveShellOptions = {}) {
		this.renderer = renderer;
		this.sidebarWidthValue = clampOpenTUISidebarWidth(options.sidebarWidth ?? OPEN_TUI_LAYOUT.sidebarDefaultWidth);
		this.root = new BoxRenderable(renderer, {
			id: "bone-shell",
			width: "100%",
			height: "100%",
			flexDirection: "column",
			backgroundColor: OPEN_TUI_COLORS.page,
		});
		this.bodyRoot = new BoxRenderable(renderer, {
			id: "bone-shell-body",
			flexDirection: "row",
			flexGrow: 1,
			minHeight: 0,
			width: "100%",
			onMouseUp: (event) => this.endSidebarDrag(event),
			onMouseDrag: (event) => this.updateSidebarDrag(event),
			onMouseDragEnd: (event) => this.endSidebarDrag(event),
		});
		this.sidebarRoot = new BoxRenderable(renderer, {
			id: "bone-sidebar-region",
			height: "100%",
			flexShrink: 0,
			flexDirection: "column",
			overflow: "hidden",
			paddingX: 1,
			backgroundColor: OPEN_TUI_COLORS.panel,
		});
		this.separatorRoot = new BoxRenderable(renderer, {
			id: "bone-sidebar-separator",
			width: OPEN_TUI_LAYOUT.separatorWidth,
			height: "100%",
			flexShrink: 0,
			backgroundColor: OPEN_TUI_COLORS.borderSubtle,
			onMouseDown: (event) => this.beginSidebarDrag(event),
			onMouseDrag: (event) => this.updateSidebarDrag(event),
			onMouseDragEnd: (event) => this.endSidebarDrag(event),
			onMouseOver: (event) => {
				this.separatorHovered = true;
				this.refreshSeparator();
				event.stopPropagation();
			},
			onMouseOut: () => {
				this.separatorHovered = false;
				this.refreshSeparator();
			},
		});
		this.mainRoot = new BoxRenderable(renderer, {
			id: "bone-main-region",
			flexDirection: "column",
			flexGrow: 1,
			height: "100%",
			minWidth: 0,
			minHeight: 0,
			backgroundColor: OPEN_TUI_COLORS.page,
		});
		const mainInner = new BoxRenderable(renderer, {
			flexDirection: "column",
			width: "100%",
			height: "100%",
			minHeight: 0,
			paddingX: 1,
		});
		this.headerRoot = new BoxRenderable(renderer, { flexDirection: "column", flexShrink: 0 });
		this.transcript = new ScrollBoxRenderable(renderer, {
			id: "bone-transcript",
			flexGrow: 1,
			flexBasis: 0,
			width: "100%",
			minHeight: 0,
			scrollY: true,
			stickyScroll: true,
			stickyStart: "bottom",
			viewportCulling: true,
		});
		this.fixedRoot = new BoxRenderable(renderer, { flexDirection: "column", flexShrink: 0 });
		this.aboveEditorRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.editorRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.belowEditorRoot = new BoxRenderable(renderer, { flexDirection: "column" });
		this.footerRoot = new BoxRenderable(renderer, { flexDirection: "column" });

		this.fixedRoot.add(this.aboveEditorRoot);
		this.fixedRoot.add(this.editorRoot);
		this.fixedRoot.add(this.belowEditorRoot);
		this.fixedRoot.add(this.footerRoot);
		mainInner.add(this.headerRoot);
		mainInner.add(this.transcript);
		mainInner.add(this.fixedRoot);
		this.mainRoot.add(mainInner);
		this.bodyRoot.add(this.sidebarRoot);
		this.bodyRoot.add(this.separatorRoot);
		this.bodyRoot.add(this.mainRoot);
		this.root.add(this.bodyRoot);

		this.applyResponsiveLayout(renderer.width);
		this.resizeHandler = () => this.applyResponsiveLayout(renderer.width);
		renderer.on(CliRenderEvents.RESIZE, this.resizeHandler);
	}

	get layoutMode(): OpenTUILayoutMode {
		return this.layoutModeValue;
	}

	get primaryPane(): OpenTUIPrimaryPane {
		return this.activePane;
	}

	get sidebarWidth(): number {
		return this.sidebarWidthValue;
	}

	showPane(pane: OpenTUIPrimaryPane): void {
		this.activePane = pane;
		this.applyResponsiveLayout(this.renderer.width);
	}

	setSidebarWidth(width: number, persist = false): void {
		const next = clampOpenTUISidebarWidth(width);
		if (next === this.sidebarWidthValue) return;
		this.sidebarWidthValue = next;
		this.applyResponsiveLayout(this.renderer.width);
		if (persist) this.onSidebarWidthChange?.(next);
	}

	appendTranscript(node: Renderable): Renderable {
		this.transcript.add(node);
		if (this.onTranscriptContentChange) this.onTranscriptContentChange();
		else this.transcript.scrollTo(Number.MAX_SAFE_INTEGER);
		return node;
	}

	appendFixed(node: Renderable): Renderable {
		this.fixedRoot.add(node);
		return node;
	}

	setSidebar(node: Renderable | undefined): Renderable | undefined {
		this.clearChildren(this.sidebarRoot);
		if (!node) return undefined;
		this.sidebarRoot.add(node);
		return node;
	}

	clearTranscript(): void {
		this.clearChildren(this.transcript);
	}

	updateTheme(_nextTheme: Theme): void {
		// The product surface currently owns one OpenCode-derived dark palette.
	}

	scrollTranscript(deltaRows: number): void {
		if (this.onTranscriptScrollRequest) this.onTranscriptScrollRequest(deltaRows);
		else this.transcript.scrollBy(deltaRows);
	}

	getTranscriptNode(): ScrollBoxRenderable {
		return this.transcript;
	}

	getExtensionRegions(): {
		header: BoxRenderable;
		aboveEditor: BoxRenderable;
		editor: BoxRenderable;
		belowEditor: BoxRenderable;
		footer: BoxRenderable;
	} {
		return {
			header: this.headerRoot,
			aboveEditor: this.aboveEditorRoot,
			editor: this.editorRoot,
			belowEditor: this.belowEditorRoot,
			footer: this.footerRoot,
		};
	}

	dispose(): void {
		this.renderer.off(CliRenderEvents.RESIZE, this.resizeHandler);
		if (this.sidebarPersistTimer) clearTimeout(this.sidebarPersistTimer);
		this.sidebarPersistTimer = undefined;
	}

	private beginSidebarDrag(event: MouseEvent): void {
		if (event.button !== 0) return;
		this.separatorDragging = true;
		this.refreshSeparator();
		this.updateSidebarWidthFromPointer(event.x);
		event.preventDefault();
		event.stopPropagation();
	}

	private updateSidebarDrag(event: MouseEvent): void {
		if (!this.separatorDragging) return;
		this.updateSidebarWidthFromPointer(event.x);
		this.scheduleSidebarWidthPersist();
		event.preventDefault();
		event.stopPropagation();
	}

	private endSidebarDrag(event: MouseEvent): void {
		if (!this.separatorDragging) return;
		this.updateSidebarWidthFromPointer(event.x);
		this.separatorDragging = false;
		this.refreshSeparator();
		this.persistSidebarWidth();
		event.preventDefault();
		event.stopPropagation();
	}

	private scheduleSidebarWidthPersist(): void {
		if (this.sidebarPersistTimer) clearTimeout(this.sidebarPersistTimer);
		this.sidebarPersistTimer = setTimeout(() => this.persistSidebarWidth(), 160);
	}

	private persistSidebarWidth(): void {
		if (this.sidebarPersistTimer) clearTimeout(this.sidebarPersistTimer);
		this.sidebarPersistTimer = undefined;
		this.onSidebarWidthChange?.(this.sidebarWidthValue);
	}

	private updateSidebarWidthFromPointer(pointerX: number): void {
		const maxForMain = Math.max(
			OPEN_TUI_LAYOUT.sidebarMinWidth,
			this.renderer.width - OPEN_TUI_LAYOUT.separatorWidth - OPEN_TUI_LAYOUT.mainMinWidth,
		);
		this.setSidebarWidth(Math.min(pointerX, maxForMain));
	}

	private refreshSeparator(): void {
		this.separatorRoot.backgroundColor =
			this.separatorDragging || this.separatorHovered ? OPEN_TUI_COLORS.primary : OPEN_TUI_COLORS.borderSubtle;
	}

	private applyResponsiveLayout(width: number): void {
		this.layoutModeValue = resolveOpenTUILayoutMode(width, this.sidebarWidthValue);
		if (this.layoutModeValue === "split") {
			this.sidebarRoot.visible = true;
			this.separatorRoot.visible = true;
			this.mainRoot.visible = true;
			this.sidebarRoot.width = this.sidebarWidthValue;
			this.separatorRoot.width = OPEN_TUI_LAYOUT.separatorWidth;
			this.mainRoot.width = "auto";
			this.mainRoot.flexGrow = 1;
			return;
		}
		this.separatorRoot.visible = false;
		const showSidebar = this.activePane === "sidebar";
		this.sidebarRoot.visible = showSidebar;
		this.mainRoot.visible = !showSidebar;
		if (showSidebar) this.sidebarRoot.width = "100%";
		else {
			this.mainRoot.width = "100%";
			this.mainRoot.flexGrow = 1;
		}
	}

	private clearChildren(parent: Renderable): void {
		for (const child of parent.getChildren()) child.destroyRecursively();
	}
}
