import type {
	BoneContainerNode,
	BoneNode,
	BoneRenderContext,
	BoneScrollViewNode,
	BoneUnsubscribe,
	BoneView,
} from "@frelion/bone-tui";
import {
	OPEN_TUI_LAYOUT,
	type OpenTUILayoutMode,
	resolveOpenTUILayoutMode,
	resolveOpenTUISidebarWidth,
} from "./opentui-design.ts";
import { type Theme, theme } from "./theme/theme.ts";

export interface OpenTUIInteractiveShellOptions {
	sidebarWidth?: number;
	compactBreakpoint?: number;
	contentMaxWidth?: number;
	theme?: Theme;
}

export type OpenTUIPrimaryPane = "sidebar" | "main";

/** Responsive application shell with stable transcript and chrome regions. */
export class OpenTUIInteractiveShell implements BoneView {
	public onTranscriptFocusRequest?: () => void;
	private readonly requestedSidebarWidth: number | undefined;
	private readonly compactBreakpoint: number;
	private readonly contentMaxWidth: number;
	private shellTheme: Theme;
	private context: BoneRenderContext | undefined;
	private shellRoot: BoneContainerNode | undefined;
	private bodyRoot: BoneContainerNode | undefined;
	private sidebarRoot: BoneContainerNode | undefined;
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

	constructor(options: OpenTUIInteractiveShellOptions = {}) {
		this.requestedSidebarWidth = options.sidebarWidth;
		this.compactBreakpoint = options.compactBreakpoint ?? OPEN_TUI_LAYOUT.compactBreakpoint;
		this.contentMaxWidth = options.contentMaxWidth ?? OPEN_TUI_LAYOUT.contentMaxWidth;
		this.shellTheme = options.theme ?? theme;
	}

	get layoutMode(): OpenTUILayoutMode {
		return this.layoutModeValue;
	}

	get primaryPane(): OpenTUIPrimaryPane {
		return this.activePane;
	}

	mount(context: BoneRenderContext): BoneNode {
		if (this.shellRoot) throw new Error("OpenTUIInteractiveShell is already mounted");
		this.context = context;
		const root = context.createBox({ width: "100%", height: "100%", flexDirection: "column" });
		const body = context.createBox({ flexDirection: "row", flexGrow: 1, minHeight: 0, width: "100%" });
		const sidebar = context.createBox({
			height: "100%",
			flexShrink: 0,
			flexDirection: "column",
			overflow: "hidden",
			paddingX: 1,
			backgroundColor: this.shellTheme.getBgColor("userMessageBg"),
		});
		const main = context.createBox({
			flexDirection: "column",
			flexGrow: 1,
			height: "100%",
			minWidth: 0,
			minHeight: 0,
			alignItems: "center",
		});
		const mainInner = context.createBox({
			flexDirection: "column",
			width: "100%",
			maxWidth: this.contentMaxWidth,
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
		body.append(main);
		root.append(body);

		this.shellRoot = root;
		this.bodyRoot = body;
		this.sidebarRoot = sidebar;
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

	appendTranscript(view: BoneView): BoneNode {
		const node = view.mount(this.requireContext());
		this.requireTranscript().append(node);
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

	updateTheme(nextTheme: Theme): void {
		this.shellTheme = nextTheme;
		this.sidebarRoot?.updateStyle({ backgroundColor: nextTheme.getBgColor("userMessageBg") });
	}

	scrollTranscript(deltaRows: number): void {
		this.requireTranscript().scrollBy(deltaRows);
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
	}

	private applyResponsiveLayout(width: number): void {
		const sidebar = this.sidebarRoot;
		const main = this.mainRoot;
		const body = this.bodyRoot;
		if (!sidebar || !main || !body) return;
		this.layoutModeValue = width < this.compactBreakpoint ? "single" : resolveOpenTUILayoutMode(width);
		if (this.layoutModeValue === "split") {
			sidebar.visible = true;
			main.visible = true;
			sidebar.updateLayout({ width: this.requestedSidebarWidth ?? resolveOpenTUISidebarWidth(width) });
			main.updateLayout({ width: "auto", flexGrow: 1 });
			body.updateStyle({ gap: 1 });
			return;
		}
		body.updateStyle({ gap: 0 });
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
