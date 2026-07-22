import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneScrollViewNode, BoneView } from "@frelion/bone-tui";
import { type Theme, theme } from "./theme/theme.ts";

export interface OpenTUIInteractiveShellOptions {
	sidebarWidth?: number;
	minimumMainWidth?: number;
	theme?: Theme;
}

/** Structured OpenTUI root for the interactive transcript vertical slice. */
export class OpenTUIInteractiveShell implements BoneView {
	public onTranscriptFocusRequest?: () => void;
	private readonly sidebarWidth: number;
	private readonly minimumMainWidth: number;
	private shellTheme: Theme;
	private context: BoneRenderContext | undefined;
	private shellRoot: BoneContainerNode | undefined;
	private sidebarRoot: BoneContainerNode | undefined;
	private transcriptRoot: BoneScrollViewNode | undefined;
	private fixedRoot: BoneContainerNode | undefined;
	private headerRoot: BoneContainerNode | undefined;
	private aboveEditorRoot: BoneContainerNode | undefined;
	private editorRoot: BoneContainerNode | undefined;
	private belowEditorRoot: BoneContainerNode | undefined;
	private footerRoot: BoneContainerNode | undefined;

	constructor(options: OpenTUIInteractiveShellOptions = {}) {
		this.sidebarWidth = options.sidebarWidth ?? 30;
		this.minimumMainWidth = options.minimumMainWidth ?? 44;
		this.shellTheme = options.theme ?? theme;
	}

	mount(context: BoneRenderContext): BoneNode {
		if (this.shellRoot) throw new Error("OpenTUIInteractiveShell is already mounted");
		this.context = context;
		const root = context.createBox({ width: "100%", height: "100%", flexDirection: "column" });
		const body = context.createBox({ flexDirection: "row", flexGrow: 1, minHeight: 0 });
		const sidebar = context.createBox({
			width: this.sidebarWidth,
			height: "100%",
			flexShrink: 0,
			flexDirection: "column",
			overflow: "hidden",
			border: true,
			borderStyle: "single",
			borderColor: this.shellTheme.getFgColor("borderMuted"),
		});
		const main = context.createBox({
			flexDirection: "column",
			flexGrow: 1,
			height: "100%",
			minWidth: this.minimumMainWidth,
			minHeight: 0,
		});
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
		const header = context.createBox({ flexDirection: "column" });
		const aboveEditor = context.createBox({ flexDirection: "column" });
		const editor = context.createBox({ flexDirection: "column" });
		const belowEditor = context.createBox({ flexDirection: "column" });
		const footer = context.createBox({ flexDirection: "column" });
		fixed.append(header);
		fixed.append(aboveEditor);
		fixed.append(editor);
		fixed.append(belowEditor);
		fixed.append(footer);

		main.append(transcript);
		main.append(fixed);
		body.append(sidebar);
		body.append(main);
		root.append(body);

		this.shellRoot = root;
		this.sidebarRoot = sidebar;
		this.transcriptRoot = transcript;
		this.fixedRoot = fixed;
		this.headerRoot = header;
		this.aboveEditorRoot = aboveEditor;
		this.editorRoot = editor;
		this.belowEditorRoot = belowEditor;
		this.footerRoot = footer;
		return root;
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
		this.sidebarRoot?.updateStyle({ borderColor: nextTheme.getFgColor("borderMuted") });
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
