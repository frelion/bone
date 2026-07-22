import { type CliRenderer, createCliRenderer, type KeyEvent, type Renderable } from "@opentui/core";
import { BoneNodeFactory, getNativeNode } from "./nodes.ts";
import type {
	BoneBoxOptions,
	BoneContainerNode,
	BoneDiffNode,
	BoneDiffOptions,
	BoneImageNode,
	BoneImageOptions,
	BoneInputNode,
	BoneInputOptions,
	BoneKeyEvent,
	BoneKeyListener,
	BoneMarkdownNode,
	BoneMarkdownOptions,
	BoneNode,
	BoneOverlayAnchor,
	BoneOverlayHandle,
	BoneOverlayOptions,
	BoneRenderer,
	BoneRendererOptions,
	BoneScrollViewNode,
	BoneScrollViewOptions,
	BoneSelectNode,
	BoneSelectOptions,
	BoneSpacerOptions,
	BoneTextareaNode,
	BoneTextareaOptions,
	BoneTextNode,
	BoneTextOptions,
	BoneUnsubscribe,
	BoneView,
} from "./types.ts";

function anchorLayout(anchor: BoneOverlayAnchor): Pick<BoneBoxOptions, "alignItems" | "justifyContent"> {
	const horizontal =
		anchor.endsWith("left") || anchor === "left-center"
			? "flex-start"
			: anchor.endsWith("right") || anchor === "right-center"
				? "flex-end"
				: "center";
	const vertical = anchor.startsWith("top") ? "flex-start" : anchor.startsWith("bottom") ? "flex-end" : "center";
	return { alignItems: horizontal, justifyContent: vertical };
}

class OverlayHandleImpl implements BoneOverlayHandle {
	readonly node: BoneNode;
	private readonly wrapper: BoneContainerNode;
	private readonly previousFocus: Renderable | null;
	private readonly renderer: CliRenderer;
	private readonly overlayLayer: BoneContainerNode;
	private closed = false;

	constructor(
		renderer: CliRenderer,
		overlayLayer: BoneContainerNode,
		wrapper: BoneContainerNode,
		node: BoneNode,
		previousFocus: Renderable | null,
	) {
		this.renderer = renderer;
		this.overlayLayer = overlayLayer;
		this.wrapper = wrapper;
		this.node = node;
		this.previousFocus = previousFocus;
	}

	get hidden(): boolean {
		return !this.wrapper.visible;
	}

	hide(): void {
		if (this.closed) return;
		this.wrapper.visible = false;
		if (this.renderer.currentFocusedRenderable === getNativeNode(this.node)) this.restoreFocus();
	}

	show(): void {
		if (this.closed) return;
		this.wrapper.visible = true;
	}

	focus(): void {
		if (this.closed) return;
		this.show();
		this.node.focus();
	}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		if (this.renderer.currentFocusedRenderable === getNativeNode(this.node)) this.restoreFocus();
		this.wrapper.destroy();
		if (this.overlayLayer.childCount === 0) this.overlayLayer.visible = false;
	}

	private restoreFocus(): void {
		if (this.previousFocus && !this.previousFocus.isDestroyed && this.previousFocus.visible)
			this.previousFocus.focus();
		else this.node.blur();
	}
}

export class BoneRendererImpl implements BoneRenderer {
	readonly root: BoneContainerNode;
	readonly content: BoneContainerNode;
	readonly overlays: BoneContainerNode;
	protected readonly nativeRenderer: CliRenderer;
	private readonly nodes: BoneNodeFactory;
	private readonly keyListeners = new Set<BoneKeyListener>();
	private readonly keyHandler: (event: KeyEvent) => void;

	constructor(nativeRenderer: CliRenderer) {
		this.nativeRenderer = nativeRenderer;
		this.nodes = new BoneNodeFactory(nativeRenderer);
		this.root = this.nodes.createBox({ id: "bone-root", width: "100%", height: "100%" });
		this.content = this.nodes.createBox({
			id: "bone-content",
			width: "100%",
			height: "100%",
			flexDirection: "column",
		});
		this.overlays = this.nodes.createBox({
			id: "bone-overlays",
			position: "absolute",
			top: 0,
			right: 0,
			bottom: 0,
			left: 0,
			zIndex: 10_000,
		});
		this.overlays.visible = false;
		this.root.append(this.content);
		this.root.append(this.overlays);
		nativeRenderer.root.add(getNativeNode(this.root));
		this.keyHandler = (event) => {
			const boneEvent: BoneKeyEvent = {
				name: event.name,
				ctrl: event.ctrl,
				meta: event.meta,
				shift: event.shift,
				option: event.option,
				sequence: event.sequence,
				raw: event.raw,
				eventType: event.eventType,
				source: event.source,
				preventDefault: () => event.preventDefault(),
				stopPropagation: () => event.stopPropagation(),
			};
			for (const listener of this.keyListeners) listener(boneEvent);
		};
		nativeRenderer.keyInput.on("keypress", this.keyHandler);
		nativeRenderer.keyInput.on("keyrelease", this.keyHandler);
	}

	get width(): number {
		return this.nativeRenderer.width;
	}

	get height(): number {
		return this.nativeRenderer.height;
	}

	get running(): boolean {
		return this.nativeRenderer.isRunning;
	}

	createBox(options: BoneBoxOptions = {}): BoneContainerNode {
		return this.nodes.createBox(options);
	}

	createText(options: BoneTextOptions = {}): BoneTextNode {
		return this.nodes.createText(options);
	}

	createScrollView(options: BoneScrollViewOptions = {}): BoneScrollViewNode {
		return this.nodes.createScrollView(options);
	}

	createMarkdown(options: BoneMarkdownOptions = {}): BoneMarkdownNode {
		return this.nodes.createMarkdown(options);
	}

	createDiff(options: BoneDiffOptions = {}): BoneDiffNode {
		return this.nodes.createDiff(options);
	}

	createImage(options: BoneImageOptions): BoneImageNode {
		return this.nodes.createImage(options);
	}

	createTextarea(options: BoneTextareaOptions = {}): BoneTextareaNode {
		return this.nodes.createTextarea(options);
	}

	createInput(options: BoneInputOptions = {}): BoneInputNode {
		return this.nodes.createInput(options);
	}

	createSelect<Value = string>(options: BoneSelectOptions<Value> = {}): BoneSelectNode<Value> {
		return this.nodes.createSelect(options);
	}

	createSpacer(options: BoneSpacerOptions = {}): BoneNode {
		return this.nodes.createSpacer(options);
	}

	mount(view: BoneView, parent: BoneContainerNode = this.content): BoneNode {
		const node = view.mount(this);
		parent.append(node);
		return node;
	}

	showOverlay(node: BoneNode, options: BoneOverlayOptions = {}): BoneOverlayHandle {
		const margin = options.margin ?? 0;
		const wrapper = this.createBox({
			id: `bone-overlay-${node.id}`,
			position: "absolute",
			top: 0,
			right: 0,
			bottom: 0,
			left: 0,
			padding: margin,
			zIndex: options.zIndex ?? 0,
			...anchorLayout(options.anchor ?? "center"),
		});
		node.updateLayout({
			width: options.width,
			height: options.height,
			maxWidth: options.maxWidth,
			maxHeight: options.maxHeight,
		});
		const previousFocus = this.nativeRenderer.currentFocusedRenderable;
		wrapper.append(node);
		this.overlays.visible = true;
		this.overlays.append(wrapper);
		const handle = new OverlayHandleImpl(this.nativeRenderer, this.overlays, wrapper, node, previousFocus);
		if (options.captureFocus !== false) handle.focus();
		return handle;
	}

	focus(node: BoneNode): void {
		getNativeNode(node).focus();
	}

	blur(node: BoneNode): void {
		getNativeNode(node).blur();
	}

	onKey(listener: BoneKeyListener): BoneUnsubscribe {
		this.keyListeners.add(listener);
		return () => this.keyListeners.delete(listener);
	}

	requestRender(): void {
		this.nativeRenderer.requestRender();
	}

	resize(width: number, height: number): void {
		this.nativeRenderer.resize(width, height);
	}

	start(): void {
		this.nativeRenderer.start();
	}

	stop(): void {
		this.nativeRenderer.stop();
	}

	destroy(): void {
		this.keyListeners.clear();
		this.nativeRenderer.keyInput.off("keypress", this.keyHandler);
		this.nativeRenderer.keyInput.off("keyrelease", this.keyHandler);
		this.nativeRenderer.destroy();
	}

	idle(): Promise<void> {
		return this.nativeRenderer.idle();
	}
}

export async function createBoneRenderer(options: BoneRendererOptions = {}): Promise<BoneRenderer> {
	const renderer = await createCliRenderer({
		screenMode: options.screenMode ?? "alternate-screen",
		footerHeight: options.footerHeight,
		useMouse: options.useMouse ?? true,
		useKittyKeyboard: options.useKittyKeyboard === false ? null : {},
		exitOnCtrlC: options.exitOnCtrlC ?? false,
		clearOnShutdown: options.clearOnShutdown ?? true,
		targetFps: options.targetFps ?? 60,
		backgroundColor: options.backgroundColor,
	});
	return new BoneRendererImpl(renderer);
}
