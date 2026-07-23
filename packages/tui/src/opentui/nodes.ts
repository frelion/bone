import {
	BoxRenderable,
	DiffRenderable,
	FrameBufferRenderable,
	InputRenderable,
	InputRenderableEvents,
	MarkdownRenderable,
	type MouseEvent,
	type Renderable,
	RenderableEvents,
	type RenderContext,
	ScrollBoxRenderable,
	SelectRenderable,
	SelectRenderableEvents,
	SyntaxStyle,
	TextAttributes,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import type {
	BoneBoxOptions,
	BoneBoxStyle,
	BoneContainerNode,
	BoneDiffNode,
	BoneDiffOptions,
	BoneDiffStyle,
	BoneImageNode,
	BoneImageOptions,
	BoneInputAction,
	BoneInputNode,
	BoneInputOptions,
	BoneKeyBinding,
	BoneLayout,
	BoneMarkdownNode,
	BoneMarkdownOptions,
	BoneMarkdownStyle,
	BoneNode,
	BoneNodeOptions,
	BoneScrollViewNode,
	BoneScrollViewOptions,
	BoneSelectAction,
	BoneSelectItem,
	BoneSelectNode,
	BoneSelectOptions,
	BoneSelectStyle,
	BoneSpacerOptions,
	BoneTextareaNode,
	BoneTextareaOptions,
	BoneTextareaStyle,
	BoneTextNode,
	BoneTextOptions,
	BoneTextStyle,
} from "./types.ts";

declare const Bun: {
	FFI: {
		ptr(value: ArrayBufferView): number;
	};
};

const nativeNodes = new WeakMap<BoneNode, Renderable>();

export function getNativeNode(node: BoneNode): Renderable {
	const native = nativeNodes.get(node);
	if (!native) throw new Error("Node was not created by this Bone renderer");
	return native;
}

function applyLayout(node: Renderable, layout: BoneLayout): void {
	if (layout.width !== undefined) node.width = layout.width;
	if (layout.height !== undefined) node.height = layout.height;
	if (layout.minWidth !== undefined && layout.minWidth !== "auto") node.minWidth = layout.minWidth;
	if (layout.minHeight !== undefined && layout.minHeight !== "auto") node.minHeight = layout.minHeight;
	if (layout.maxWidth !== undefined && layout.maxWidth !== "auto") node.maxWidth = layout.maxWidth;
	if (layout.maxHeight !== undefined && layout.maxHeight !== "auto") node.maxHeight = layout.maxHeight;
	if (layout.flexGrow !== undefined) node.flexGrow = layout.flexGrow;
	if (layout.flexShrink !== undefined) node.flexShrink = layout.flexShrink;
	if (layout.flexBasis !== undefined) node.flexBasis = layout.flexBasis;
	if (layout.flexDirection !== undefined) node.flexDirection = layout.flexDirection;
	if (layout.flexWrap !== undefined) node.flexWrap = layout.flexWrap;
	if (layout.alignItems !== undefined) node.alignItems = layout.alignItems;
	if (layout.alignSelf !== undefined) node.alignSelf = layout.alignSelf;
	if (layout.justifyContent !== undefined) node.justifyContent = layout.justifyContent;
	if (layout.position !== undefined) node.position = layout.position;
	if (layout.overflow !== undefined) node.overflow = layout.overflow;
	if (layout.top !== undefined) node.top = layout.top;
	if (layout.right !== undefined) node.right = layout.right;
	if (layout.bottom !== undefined) node.bottom = layout.bottom;
	if (layout.left !== undefined) node.left = layout.left;
	if (layout.margin !== undefined) node.margin = layout.margin;
	if (layout.marginX !== undefined) node.marginX = layout.marginX;
	if (layout.marginY !== undefined) node.marginY = layout.marginY;
	if (layout.marginTop !== undefined) node.marginTop = layout.marginTop;
	if (layout.marginRight !== undefined) node.marginRight = layout.marginRight;
	if (layout.marginBottom !== undefined) node.marginBottom = layout.marginBottom;
	if (layout.marginLeft !== undefined) node.marginLeft = layout.marginLeft;
	if (layout.padding !== undefined) node.padding = layout.padding;
	if (layout.paddingX !== undefined) node.paddingX = layout.paddingX;
	if (layout.paddingY !== undefined) node.paddingY = layout.paddingY;
	if (layout.paddingTop !== undefined) node.paddingTop = layout.paddingTop;
	if (layout.paddingRight !== undefined) node.paddingRight = layout.paddingRight;
	if (layout.paddingBottom !== undefined) node.paddingBottom = layout.paddingBottom;
	if (layout.paddingLeft !== undefined) node.paddingLeft = layout.paddingLeft;
	if (layout.zIndex !== undefined) node.zIndex = layout.zIndex;
}

function withoutBoneInteractions<Options extends BoneNodeOptions>(
	options: Options,
): Omit<
	Options,
	"onMouseDown" | "onMouseUp" | "onMouseDrag" | "onMouseDragEnd" | "onMouseOver" | "onMouseOut" | "onMouseScroll"
> {
	const {
		onMouseDown: _onMouseDown,
		onMouseUp: _onMouseUp,
		onMouseDrag: _onMouseDrag,
		onMouseDragEnd: _onMouseDragEnd,
		onMouseOver: _onMouseOver,
		onMouseOut: _onMouseOut,
		onMouseScroll: _onMouseScroll,
		...nativeOptions
	} = options;
	return nativeOptions;
}

abstract class NodeFacade<T extends Renderable> implements BoneNode {
	protected readonly native: T;

	constructor(native: T, layout: BoneNodeOptions) {
		this.native = native;
		nativeNodes.set(this, native);
		applyLayout(native, layout);
		if (layout.onMouseDown) native.onMouseDown = (event) => layout.onMouseDown?.(toBoneMouseEvent(event));
		if (layout.onMouseUp) native.onMouseUp = (event) => layout.onMouseUp?.(toBoneMouseEvent(event));
		if (layout.onMouseDrag) native.onMouseDrag = (event) => layout.onMouseDrag?.(toBoneMouseEvent(event));
		if (layout.onMouseDragEnd) native.onMouseDragEnd = (event) => layout.onMouseDragEnd?.(toBoneMouseEvent(event));
		if (layout.onMouseOver) native.onMouseOver = (event) => layout.onMouseOver?.(toBoneMouseEvent(event));
		if (layout.onMouseOut) native.onMouseOut = (event) => layout.onMouseOut?.(toBoneMouseEvent(event));
		if (layout.onMouseScroll) native.onMouseScroll = (event) => layout.onMouseScroll?.(toBoneMouseEvent(event));
	}

	get id(): string {
		return this.native.id;
	}

	get visible(): boolean {
		return this.native.visible;
	}

	set visible(value: boolean) {
		this.native.visible = value;
	}

	get destroyed(): boolean {
		return this.native.isDestroyed;
	}

	get effectivelyVisible(): boolean {
		let current: Renderable | null = this.native;
		while (current) {
			if (!current.visible || current.isDestroyed) return false;
			current = current.parent;
		}
		return true;
	}

	get screenX(): number {
		return this.native.screenX;
	}

	get screenY(): number {
		return this.native.screenY;
	}

	get width(): number {
		return this.native.width;
	}

	get height(): number {
		return this.native.height;
	}

	updateLayout(layout: BoneLayout): void {
		applyLayout(this.native, layout);
	}

	focus(): void {
		this.native.focus();
	}

	blur(): void {
		this.native.blur();
	}

	requestRender(): void {
		this.native.requestRender();
	}

	destroy(): void {
		this.native.destroyRecursively();
	}
}

function toBoneMouseEvent(event: MouseEvent) {
	return {
		type: event.type,
		x: event.x,
		y: event.y,
		button: event.button,
		shift: event.modifiers.shift,
		alt: event.modifiers.alt,
		ctrl: event.modifiers.ctrl,
		scrollDirection: event.scroll?.direction,
		scrollDelta: event.scroll?.delta,
		preventDefault: () => event.preventDefault(),
		stopPropagation: () => event.stopPropagation(),
	};
}

function applyBoxStyle(node: BoxRenderable, style: BoneBoxStyle): void {
	if (style.backgroundColor !== undefined) node.backgroundColor = style.backgroundColor;
	if (style.border !== undefined) node.border = style.border;
	if (style.borderStyle !== undefined) node.borderStyle = style.borderStyle;
	if (style.borderColor !== undefined) node.borderColor = style.borderColor;
	if (style.title !== undefined) node.title = style.title;
	if (style.gap !== undefined) node.gap = style.gap;
	if (style.rowGap !== undefined) node.rowGap = style.rowGap;
	if (style.columnGap !== undefined) node.columnGap = style.columnGap;
}

class ContainerFacade<T extends BoxRenderable = BoxRenderable> extends NodeFacade<T> implements BoneContainerNode {
	constructor(native: T, options: BoneBoxOptions) {
		super(native, options);
		applyBoxStyle(native, options);
	}

	append(child: BoneNode): void {
		this.native.add(getNativeNode(child));
	}

	remove(child: BoneNode): void {
		this.native.remove(getNativeNode(child));
	}

	clear(): void {
		for (const child of this.native.getChildren()) child.destroyRecursively();
	}

	get childCount(): number {
		return this.native.getChildrenCount();
	}

	updateStyle(style: BoneBoxStyle): void {
		applyBoxStyle(this.native, style);
	}
}

function textAttributes(style: BoneTextStyle): number | undefined {
	const properties = [style.bold, style.italic, style.underline, style.dim, style.strikethrough];
	if (properties.every((value) => value === undefined)) return undefined;
	let attributes = TextAttributes.NONE;
	if (style.bold) attributes |= TextAttributes.BOLD;
	if (style.italic) attributes |= TextAttributes.ITALIC;
	if (style.underline) attributes |= TextAttributes.UNDERLINE;
	if (style.dim) attributes |= TextAttributes.DIM;
	if (style.strikethrough) attributes |= TextAttributes.STRIKETHROUGH;
	return attributes;
}

function applyTextStyle(node: TextRenderable, style: BoneTextStyle): void {
	if (style.fg !== undefined) node.fg = style.fg;
	if (style.bg !== undefined) node.bg = style.bg;
	if (style.selectable !== undefined) node.selectable = style.selectable;
	if (style.wrapMode !== undefined) node.wrapMode = style.wrapMode;
	if (style.truncate !== undefined) node.truncate = style.truncate;
	const attributes = textAttributes(style);
	if (attributes !== undefined) node.attributes = attributes;
}

class TextFacade extends NodeFacade<TextRenderable> implements BoneTextNode {
	constructor(native: TextRenderable, options: BoneTextOptions) {
		super(native, options);
		applyTextStyle(native, options);
	}

	get content(): string {
		return this.native.plainText;
	}

	set content(value: string) {
		this.native.content = value;
	}

	updateStyle(style: BoneTextStyle): void {
		applyTextStyle(this.native, style);
	}
}

class ScrollViewFacade extends ContainerFacade<ScrollBoxRenderable> implements BoneScrollViewNode {
	get scrollTop(): number {
		return this.native.scrollTop;
	}

	set scrollTop(value: number) {
		this.native.scrollTop = value;
	}

	get scrollLeft(): number {
		return this.native.scrollLeft;
	}

	set scrollLeft(value: number) {
		this.native.scrollLeft = value;
	}

	get viewportHeight(): number {
		return this.native.height;
	}

	get scrollHeight(): number {
		return this.native.scrollHeight;
	}

	get scrollWidth(): number {
		return this.native.scrollWidth;
	}

	scrollBy(delta: number | { x: number; y: number }): void {
		this.native.scrollBy(delta);
	}

	scrollTo(position: number | { x: number; y: number }): void {
		this.native.scrollTo(position);
	}
}

class MarkdownFacade extends NodeFacade<MarkdownRenderable> implements BoneMarkdownNode {
	private readonly syntaxStyle: SyntaxStyle;

	constructor(native: MarkdownRenderable, options: BoneMarkdownOptions, syntaxStyle: SyntaxStyle) {
		super(native, options);
		this.syntaxStyle = syntaxStyle;
		native.once(RenderableEvents.DESTROYED, () => this.syntaxStyle.destroy());
		this.updateStyle(options);
	}

	get content(): string {
		return this.native.content;
	}

	set content(value: string) {
		this.native.content = value;
	}

	get streaming(): boolean {
		return this.native.streaming;
	}

	set streaming(value: boolean) {
		this.native.streaming = value;
	}

	updateStyle(style: BoneMarkdownStyle): void {
		if (style.fg !== undefined) this.native.fg = style.fg;
		if (style.bg !== undefined) this.native.bg = style.bg;
		if (style.conceal !== undefined) this.native.conceal = style.conceal;
		if (style.concealCode !== undefined) this.native.concealCode = style.concealCode;
	}
}

function applyDiffStyle(node: DiffRenderable, style: BoneDiffStyle): void {
	if (style.fg !== undefined) node.fg = style.fg;
	if (style.view !== undefined) node.view = style.view;
	if (style.filetype !== undefined) node.filetype = style.filetype;
	if (style.wrapMode !== undefined) node.wrapMode = style.wrapMode;
	if (style.showLineNumbers !== undefined) node.showLineNumbers = style.showLineNumbers;
	if (style.addedBg !== undefined) node.addedBg = style.addedBg;
	if (style.removedBg !== undefined) node.removedBg = style.removedBg;
	if (style.contextBg !== undefined) node.contextBg = style.contextBg;
	if (style.addedSignColor !== undefined) node.addedSignColor = style.addedSignColor;
	if (style.removedSignColor !== undefined) node.removedSignColor = style.removedSignColor;
}

class DiffFacade extends NodeFacade<DiffRenderable> implements BoneDiffNode {
	constructor(native: DiffRenderable, options: BoneDiffOptions) {
		super(native, options);
		applyDiffStyle(native, options);
	}

	get diff(): string {
		return this.native.diff;
	}

	set diff(value: string) {
		this.native.diff = value;
	}

	updateStyle(style: BoneDiffStyle): void {
		applyDiffStyle(this.native, style);
	}
}

class RawImageRenderable extends FrameBufferRenderable {
	private pixels: Uint8Array;
	private pixelWidthValue: number;
	private pixelHeightValue: number;

	constructor(context: RenderContext, options: BoneImageOptions) {
		super(context, {
			...withoutBoneInteractions(options),
			width: options.terminalWidth,
			height: options.terminalHeight,
		});
		this.pixels = options.pixels;
		this.pixelWidthValue = options.pixelWidth;
		this.pixelHeightValue = options.pixelHeight;
		this.redraw();
	}

	get pixelWidth(): number {
		return this.pixelWidthValue;
	}

	get pixelHeight(): number {
		return this.pixelHeightValue;
	}

	setPixels(pixels: Uint8Array, pixelWidth: number, pixelHeight: number): void {
		this.pixels = pixels;
		this.pixelWidthValue = pixelWidth;
		this.pixelHeightValue = pixelHeight;
		this.redraw();
		this.requestRender();
	}

	private redraw(): void {
		const expectedLength = this.pixelWidthValue * this.pixelHeightValue * 4;
		if (this.pixelWidthValue <= 0 || this.pixelHeightValue <= 0 || this.pixels.length !== expectedLength) {
			throw new RangeError(`Expected ${expectedLength} RGBA bytes, received ${this.pixels.length}`);
		}
		this.frameBuffer.clear();
		this.frameBuffer.drawSuperSampleBuffer(
			0,
			0,
			Bun.FFI.ptr(this.pixels),
			this.pixels.length,
			"rgba8unorm",
			this.pixelWidthValue * 4,
		);
	}
}

class ImageFacade extends NodeFacade<RawImageRenderable> implements BoneImageNode {
	get pixelWidth(): number {
		return this.native.pixelWidth;
	}

	get pixelHeight(): number {
		return this.native.pixelHeight;
	}

	setPixels(pixels: Uint8Array, pixelWidth: number, pixelHeight: number): void {
		this.native.setPixels(pixels, pixelWidth, pixelHeight);
	}
}

function applyTextareaStyle(node: TextareaRenderable, style: BoneTextareaStyle): void {
	if (style.textColor !== undefined) node.textColor = style.textColor;
	if (style.backgroundColor !== undefined) node.backgroundColor = style.backgroundColor;
	if (style.focusedTextColor !== undefined) node.focusedTextColor = style.focusedTextColor;
	if (style.focusedBackgroundColor !== undefined) node.focusedBackgroundColor = style.focusedBackgroundColor;
	if (style.placeholderColor !== undefined) node.placeholderColor = style.placeholderColor;
	if (style.wrapMode !== undefined) node.wrapMode = style.wrapMode;
	if (style.showCursor !== undefined) node.showCursor = style.showCursor;
	if (style.cursorColor !== undefined) node.cursorColor = style.cursorColor;
}

class TextareaFacade extends NodeFacade<TextareaRenderable> implements BoneTextareaNode {
	constructor(native: TextareaRenderable, options: BoneTextareaOptions) {
		super(native, options);
		applyTextareaStyle(native, options);
	}

	get value(): string {
		return this.native.plainText;
	}

	set value(value: string) {
		this.native.setText(value);
	}

	get placeholder(): string | null {
		const value = this.native.placeholder;
		return typeof value === "string" ? value : null;
	}

	set placeholder(value: string | null) {
		this.native.placeholder = value;
	}

	get cursorOffset(): number {
		return this.native.cursorOffset;
	}

	setCursorOffset(offset: number): void {
		this.native.cursorOffset = offset;
	}

	insertText(text: string): void {
		this.native.insertText(text);
	}

	updateStyle(style: BoneTextareaStyle): void {
		applyTextareaStyle(this.native, style);
	}
}

function matchesBinding(
	event: { name: string; ctrl: boolean; shift: boolean; meta: boolean; super?: boolean },
	binding: BoneKeyBinding<string>,
	aliases: Record<string, string> = {},
): boolean {
	const name =
		aliases[binding.name] ?? (binding.name === "esc" ? "escape" : binding.name === "enter" ? "return" : binding.name);
	return (
		event.name === name &&
		event.ctrl === (binding.ctrl ?? false) &&
		event.shift === (binding.shift ?? false) &&
		event.meta === (binding.meta ?? false) &&
		(event.super ?? false) === (binding.super ?? false)
	);
}

function cancelBindings<Action extends string>(
	bindings: BoneKeyBinding<Action>[] | undefined,
): BoneKeyBinding<Action | "cancel">[] {
	const custom = bindings?.filter((binding) => binding.action === "cancel") ?? [];
	return custom.length > 0 ? custom : [{ name: "escape", action: "cancel" }];
}

class InputFacade extends NodeFacade<InputRenderable> implements BoneInputNode {
	constructor(native: InputRenderable, options: BoneInputOptions) {
		super(native, options);
		applyTextareaStyle(native, options);
	}

	get value(): string {
		return this.native.value;
	}

	set value(value: string) {
		this.native.value = value;
	}

	get placeholder(): string {
		return this.native.placeholder;
	}

	set placeholder(value: string) {
		this.native.placeholder = value;
	}

	get minLength(): number {
		return this.native.minLength;
	}

	set minLength(value: number) {
		this.native.minLength = value;
	}

	get maxLength(): number {
		return this.native.maxLength;
	}

	set maxLength(value: number) {
		this.native.maxLength = value;
	}

	get cursorOffset(): number {
		return this.native.cursorOffset;
	}

	setCursorOffset(offset: number): void {
		this.native.cursorOffset = offset;
	}

	insertText(text: string): void {
		this.native.insertText(text);
	}

	submit(): boolean {
		return this.native.submit();
	}

	updateStyle(style: BoneTextareaStyle): void {
		applyTextareaStyle(this.native, style);
	}
}

function applySelectStyle(node: SelectRenderable, style: BoneSelectStyle): void {
	if (style.backgroundColor !== undefined) node.backgroundColor = style.backgroundColor;
	if (style.textColor !== undefined) node.textColor = style.textColor;
	if (style.focusedBackgroundColor !== undefined) node.focusedBackgroundColor = style.focusedBackgroundColor;
	if (style.focusedTextColor !== undefined) node.focusedTextColor = style.focusedTextColor;
	if (style.selectedBackgroundColor !== undefined) node.selectedBackgroundColor = style.selectedBackgroundColor;
	if (style.selectedTextColor !== undefined) node.selectedTextColor = style.selectedTextColor;
	if (style.descriptionColor !== undefined) node.descriptionColor = style.descriptionColor;
	if (style.selectedDescriptionColor !== undefined) node.selectedDescriptionColor = style.selectedDescriptionColor;
	if (style.showScrollIndicator !== undefined) node.showScrollIndicator = style.showScrollIndicator;
	if (style.wrapSelection !== undefined) node.wrapSelection = style.wrapSelection;
	if (style.showDescription !== undefined) node.showDescription = style.showDescription;
	if (style.showSelectionIndicator !== undefined) node.showSelectionIndicator = style.showSelectionIndicator;
	if (style.itemSpacing !== undefined) node.itemSpacing = style.itemSpacing;
	if (style.fastScrollStep !== undefined) node.fastScrollStep = style.fastScrollStep;
}

class SelectFacade<Value> extends NodeFacade<SelectRenderable> implements BoneSelectNode<Value> {
	private currentItems: BoneSelectItem<Value>[];

	constructor(native: SelectRenderable, options: BoneSelectOptions<Value>) {
		super(native, options);
		this.currentItems = options.items ?? [];
		applySelectStyle(native, options);
	}

	get items(): BoneSelectItem<Value>[] {
		return [...this.currentItems];
	}

	set items(items: BoneSelectItem<Value>[]) {
		this.currentItems = [...items];
		this.native.options = toNativeSelectOptions(items);
	}

	get selectedIndex(): number {
		return this.native.getSelectedIndex();
	}

	set selectedIndex(index: number) {
		this.native.setSelectedIndex(index);
	}

	get selectedItem(): BoneSelectItem<Value> | null {
		return this.currentItems[this.selectedIndex] ?? null;
	}

	moveUp(steps?: number): void {
		if (this.currentItems.length === 0) return;
		this.native.moveUp(steps);
	}

	moveDown(steps?: number): void {
		if (this.currentItems.length === 0) return;
		this.native.moveDown(steps);
	}

	confirm(): void {
		this.native.selectCurrent();
	}

	updateStyle(style: BoneSelectStyle): void {
		applySelectStyle(this.native, style);
	}
}

function toNativeSelectOptions<Value>(items: BoneSelectItem<Value>[]) {
	return items.map((item) => ({ name: item.label, description: item.description ?? "" }));
}

export class BoneNodeFactory {
	private readonly context: RenderContext;

	constructor(context: RenderContext) {
		this.context = context;
	}

	createBox(options: BoneBoxOptions = {}): BoneContainerNode {
		return new ContainerFacade(new BoxRenderable(this.context, withoutBoneInteractions(options)), options);
	}

	createText(options: BoneTextOptions = {}): BoneTextNode {
		return new TextFacade(new TextRenderable(this.context, withoutBoneInteractions(options)), options);
	}

	createScrollView(options: BoneScrollViewOptions = {}): BoneScrollViewNode {
		return new ScrollViewFacade(new ScrollBoxRenderable(this.context, withoutBoneInteractions(options)), options);
	}

	createMarkdown(options: BoneMarkdownOptions = {}): BoneMarkdownNode {
		const syntaxStyle = SyntaxStyle.fromStyles({
			default: { fg: options.fg ?? "#d4d4d4" },
			"markup.heading": { fg: options.fg ?? "#d4d4d4", bold: true },
			"markup.link": { fg: "#5fafff", underline: true },
			"markup.raw": { fg: "#87d787" },
		});
		const native = new MarkdownRenderable(this.context, { ...withoutBoneInteractions(options), syntaxStyle });
		return new MarkdownFacade(native, options, syntaxStyle);
	}

	createDiff(options: BoneDiffOptions = {}): BoneDiffNode {
		return new DiffFacade(new DiffRenderable(this.context, withoutBoneInteractions(options)), options);
	}

	createImage(options: BoneImageOptions): BoneImageNode {
		return new ImageFacade(new RawImageRenderable(this.context, options), options);
	}

	createTextarea(options: BoneTextareaOptions = {}): BoneTextareaNode {
		const bindings = options.keyBindings?.filter((binding) => binding.action !== "cancel");
		const cancels = cancelBindings(options.keyBindings);
		const native = new TextareaRenderable(this.context, {
			...withoutBoneInteractions(options),
			keyBindings: bindings as BoneKeyBinding<Exclude<BoneInputAction, "cancel">>[] | undefined,
			keyAliasMap: options.keyAliases,
			onSubmit: () => options.onSubmit?.(native.plainText),
			onContentChange: () => options.onChange?.(native.plainText),
			onKeyDown: (event) => {
				if (!cancels.some((binding) => matchesBinding(event, binding, options.keyAliases))) return;
				event.preventDefault();
				event.stopPropagation();
				options.onCancel?.();
			},
		});
		return new TextareaFacade(native, options);
	}

	createInput(options: BoneInputOptions = {}): BoneInputNode {
		const bindings = options.keyBindings?.filter((binding) => binding.action !== "cancel");
		const cancels = cancelBindings(options.keyBindings);
		const native = new InputRenderable(this.context, {
			...withoutBoneInteractions(options),
			keyBindings: bindings as BoneKeyBinding<Exclude<BoneInputAction, "cancel">>[] | undefined,
			keyAliasMap: options.keyAliases,
			onKeyDown: (event) => {
				if (!cancels.some((binding) => matchesBinding(event, binding, options.keyAliases))) return;
				event.preventDefault();
				event.stopPropagation();
				options.onCancel?.();
			},
		});
		native.on(InputRenderableEvents.INPUT, (value: string) => options.onInput?.(value));
		native.on(InputRenderableEvents.CHANGE, (value: string) => options.onChange?.(value));
		native.on(InputRenderableEvents.ENTER, (value: string) => options.onConfirm?.(value));
		return new InputFacade(native, options);
	}

	createSelect<Value = string>(options: BoneSelectOptions<Value> = {}): BoneSelectNode<Value> {
		const bindings = options.keyBindings
			?.filter((binding) => binding.action !== "cancel")
			.map((binding) => ({ ...binding, action: binding.action === "confirm" ? "select-current" : binding.action }));
		const cancels = cancelBindings(options.keyBindings);
		const items = options.items ?? [];
		const native = new SelectRenderable(this.context, {
			...withoutBoneInteractions(options),
			options: toNativeSelectOptions(items),
			keyBindings: bindings as
				| BoneKeyBinding<Exclude<BoneSelectAction, "confirm" | "cancel"> | "select-current">[]
				| undefined,
			keyAliasMap: options.keyAliases,
			onKeyDown: (event) => {
				if (!cancels.some((binding) => matchesBinding(event, binding, options.keyAliases))) return;
				event.preventDefault();
				event.stopPropagation();
				options.onCancel?.();
			},
		});
		const facade = new SelectFacade(native, options);
		native.on(SelectRenderableEvents.SELECTION_CHANGED, (index: number) => {
			const item = facade.items[index];
			if (item) options.onChange?.(item, index);
		});
		native.on(SelectRenderableEvents.ITEM_SELECTED, (index: number) => {
			const item = facade.items[index];
			if (item) options.onConfirm?.(item, index);
		});
		return facade;
	}

	createSpacer(options: BoneSpacerOptions = {}): BoneNode {
		const size = options.size ?? 1;
		const layout: BoneLayout = { ...options };
		if (options.direction === "horizontal") layout.width = options.width ?? size;
		else layout.height = options.height ?? size;
		return new ContainerFacade(
			new BoxRenderable(this.context, { ...withoutBoneInteractions(options), ...layout, id: options.id }),
			options,
		);
	}
}
