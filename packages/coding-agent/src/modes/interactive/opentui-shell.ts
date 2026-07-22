import type {
	BoneContainerNode,
	BoneMouseEvent,
	BoneNode,
	BoneRenderContext,
	BoneScrollViewNode,
	BoneUnsubscribe,
	BoneView,
} from "@frelion/bone-tui";
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

/** Full-width conversation workspace with a persistent, draggable session rail. */
export class OpenTUIInteractiveShell implements BoneView {
	public onTranscriptFocusRequest?: () => void;
	public onTranscriptScrollRequest?: (deltaRows: number) => void;
	public onTranscriptContentChange?: () => void;
	public onSidebarWidthChange?: (width: number) => void;
	private sidebarWidthValue: number;
	private context: BoneRenderContext | undefined;
	private shellRoot: BoneContainerNode | undefined;
	private bodyRoot: BoneContainerNode | undefined;
	private sidebarRoot: BoneContainerNode | undefined;
	private separatorRoot: BoneContainerNode | undefined;
	private mainRoot: BoneContainerNode | undefined;
	private transcriptRoot: BoneScrollViewNode | undefined;
	private fixedRoot: BoneContainerNode | undefined;
	private headerRoot: BoneContainerNode | undefined;
	private aboveEditorRoot: BoneContainerNode | undefined;
	private editorRoot: BoneContainerNode | undefined;
	private belowEditorRoot: BoneContainerNode | undefined;
	private footerRoot: BoneContainerNode | undefined;
	private unsubscribeResize: BoneUnsubscribe | undefined;
	private activePane: OpenTUIPrimaryPane = "main";
	private layoutModeValue: OpenTUILayoutMode = "split";
	private separatorHovered = false;
	private separatorDragging = false;
	private sidebarPersistTimer: ReturnType<typeof setTimeout> | undefined;

	constructor(options: OpenTUIInteractiveShellOptions = {}) {
		this.sidebarWidthValue = clampOpenTUISidebarWidth(options.sidebarWidth ?? OPEN_TUI_LAYOUT.sidebarDefaultWidth);
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

	mount(context: BoneRenderContext): BoneNode {
		if (this.shellRoot) throw new Error("OpenTUIInteractiveShell is already mounted");
		this.context = context;
		const root = context.createBox({
			width: "100%",
			height: "100%",
			flexDirection: "column",
			backgroundColor: OPEN_TUI_COLORS.page,
		});
		const body = context.createBox({ flexDirection: "row", flexGrow: 1, minHeight: 0, width: "100%" });
		const sidebar = context.createBox({
			height: "100%",
			flexShrink: 0,
			flexDirection: "column",
			overflow: "hidden",
			paddingX: 1,
			backgroundColor: OPEN_TUI_COLORS.panel,
		});
		const separator = context.createBox({
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
		const main = context.createBox({
			flexDirection: "column",
			flexGrow: 1,
			height: "100%",
			minWidth: 0,
			minHeight: 0,
			backgroundColor: OPEN_TUI_COLORS.page,
			onMouseScroll: (event) => this.handleMainScroll(event),
		});
		const mainInner = context.createBox({
			flexDirection: "column",
			width: "100%",
			height: "100%",
			minHeight: 0,
			paddingX: 1,
		});
		const header = context.createBox({ flexDirection: "column", flexShrink: 0 });
		const transcript = context.createScrollView({
			flexDirection: "column",
			flexGrow: 1,
			flexBasis: 0,
			width: "100%",
			minHeight: 0,
			scrollY: true,
			stickyScroll: true,
			stickyStart: "bottom",
			viewportCulling: true,
			onMouseDown: () => this.onTranscriptFocusRequest?.(),
		});
		const fixed = context.createBox({ flexDirection: "column", flexShrink: 0 });
		const aboveEditor = context.createBox({ flexDirection: "column" });
		const editor = context.createBox({ flexDirection: "column" });
		const belowEditor = context.createBox({ flexDirection: "column" });
		const footer = context.createBox({ flexDirection: "column" });
		fixed.append(aboveEditor);
		fixed.append(editor);
		fixed.append(belowEditor);
		fixed.append(footer);

		mainInner.append(header);
		mainInner.append(transcript);
		mainInner.append(fixed);
		main.append(mainInner);
		body.append(sidebar);
		body.append(separator);
		body.append(main);
		root.append(body);

		this.shellRoot = root;
		this.bodyRoot = body;
		this.sidebarRoot = sidebar;
		this.separatorRoot = separator;
		this.mainRoot = main;
		this.transcriptRoot = transcript;
		this.fixedRoot = fixed;
		this.headerRoot = header;
		this.aboveEditorRoot = aboveEditor;
		this.editorRoot = editor;
		this.belowEditorRoot = belowEditor;
		this.footerRoot = footer;
		this.applyResponsiveLayout(context.width);
		this.unsubscribeResize = context.onResize((width) => this.applyResponsiveLayout(width));
		return root;
	}

	showPane(pane: OpenTUIPrimaryPane): void {
		this.activePane = pane;
		if (this.context) this.applyResponsiveLayout(this.context.width);
	}

	setSidebarWidth(width: number, persist = false): void {
		const next = clampOpenTUISidebarWidth(width);
		if (next === this.sidebarWidthValue) return;
		this.sidebarWidthValue = next;
		if (this.context) this.applyResponsiveLayout(this.context.width);
		if (persist) this.onSidebarWidthChange?.(next);
	}

	appendTranscript(view: BoneView): BoneNode {
		const node = view.mount(this.requireContext());
		this.requireTranscript().append(node);
		if (this.onTranscriptContentChange) this.onTranscriptContentChange();
		else this.requireTranscript().scrollTo(Number.MAX_SAFE_INTEGER);
		return node;
	}

	appendFixed(view: BoneView): BoneNode {
		const node = view.mount(this.requireContext());
		this.requireFixed().append(node);
		return node;
	}

	setSidebar(view: BoneView | undefined): BoneNode | undefined {
		const sidebar = this.requireSidebar();
		sidebar.clear();
		if (!view) return undefined;
		const node = view.mount(this.requireContext());
		sidebar.append(node);
		return node;
	}

	clearTranscript(): void {
		this.requireTranscript().clear();
	}

	updateTheme(_nextTheme: Theme): void {
		// V2 intentionally owns one OpenCode-derived dark surface system.
	}

	scrollTranscript(deltaRows: number): void {
		if (this.onTranscriptScrollRequest) this.onTranscriptScrollRequest(deltaRows);
		else this.requireTranscript().scrollBy(deltaRows);
	}

	getTranscriptNode(): BoneScrollViewNode {
		return this.requireTranscript();
	}

	getExtensionRegions(): {
		header: BoneContainerNode;
		aboveEditor: BoneContainerNode;
		editor: BoneContainerNode;
		belowEditor: BoneContainerNode;
		footer: BoneContainerNode;
	} {
		if (!this.headerRoot || !this.aboveEditorRoot || !this.editorRoot || !this.belowEditorRoot || !this.footerRoot) {
			throw new Error("OpenTUIInteractiveShell must be mounted first");
		}
		return {
			header: this.headerRoot,
			aboveEditor: this.aboveEditorRoot,
			editor: this.editorRoot,
			belowEditor: this.belowEditorRoot,
			footer: this.footerRoot,
		};
	}

	dispose(): void {
		this.unsubscribeResize?.();
		this.unsubscribeResize = undefined;
		if (this.sidebarPersistTimer) clearTimeout(this.sidebarPersistTimer);
		this.sidebarPersistTimer = undefined;
	}

	private beginSidebarDrag(event: BoneMouseEvent): void {
		this.separatorDragging = true;
		this.refreshSeparator();
		this.updateSidebarWidthFromPointer(event.x);
		event.preventDefault();
		event.stopPropagation();
	}

	private updateSidebarDrag(event: BoneMouseEvent): void {
		if (!this.separatorDragging) {
			this.separatorDragging = true;
			this.refreshSeparator();
		}
		this.updateSidebarWidthFromPointer(event.x);
		this.scheduleSidebarWidthPersist();
		event.preventDefault();
		event.stopPropagation();
	}

	private endSidebarDrag(event: BoneMouseEvent): void {
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
		const context = this.context;
		if (!context) return;
		const maxForMain = Math.max(
			OPEN_TUI_LAYOUT.sidebarMinWidth,
			context.width - OPEN_TUI_LAYOUT.separatorWidth - OPEN_TUI_LAYOUT.mainMinWidth,
		);
		this.setSidebarWidth(Math.min(pointerX, maxForMain));
	}

	private refreshSeparator(): void {
		this.separatorRoot?.updateStyle({
			backgroundColor:
				this.separatorDragging || this.separatorHovered ? OPEN_TUI_COLORS.primary : OPEN_TUI_COLORS.borderSubtle,
		});
	}

	private handleMainScroll(event: BoneMouseEvent): void {
		const direction = event.scrollDirection;
		if (direction !== "up" && direction !== "down") return;
		const delta = Math.max(1, Math.round(event.scrollDelta ?? 3));
		const rows = direction === "up" ? -delta : delta;
		if (this.onTranscriptScrollRequest) this.onTranscriptScrollRequest(rows);
		else this.transcriptRoot?.scrollBy(rows);
		event.preventDefault();
		event.stopPropagation();
	}

	private applyResponsiveLayout(width: number): void {
		const sidebar = this.sidebarRoot;
		const separator = this.separatorRoot;
		const main = this.mainRoot;
		const body = this.bodyRoot;
		if (!sidebar || !separator || !main || !body) return;
		this.layoutModeValue = resolveOpenTUILayoutMode(width, this.sidebarWidthValue);
		if (this.layoutModeValue === "split") {
			sidebar.visible = true;
			separator.visible = true;
			main.visible = true;
			sidebar.updateLayout({ width: this.sidebarWidthValue });
			separator.updateLayout({ width: OPEN_TUI_LAYOUT.separatorWidth });
			main.updateLayout({ width: "auto", flexGrow: 1 });
			return;
		}
		separator.visible = false;
		const showSidebar = this.activePane === "sidebar";
		sidebar.visible = showSidebar;
		main.visible = !showSidebar;
		if (showSidebar) sidebar.updateLayout({ width: "100%" });
		else main.updateLayout({ width: "100%", flexGrow: 1 });
	}

	private requireContext(): BoneRenderContext {
		if (!this.context) throw new Error("OpenTUIInteractiveShell must be mounted first");
		return this.context;
	}

	private requireSidebar(): BoneContainerNode {
		if (!this.sidebarRoot) throw new Error("OpenTUIInteractiveShell must be mounted first");
		return this.sidebarRoot;
	}

	private requireTranscript(): BoneScrollViewNode {
		if (!this.transcriptRoot) throw new Error("OpenTUIInteractiveShell must be mounted first");
		return this.transcriptRoot;
	}

	private requireFixed(): BoneContainerNode {
		if (!this.fixedRoot) throw new Error("OpenTUIInteractiveShell must be mounted first");
		return this.fixedRoot;
	}
}
