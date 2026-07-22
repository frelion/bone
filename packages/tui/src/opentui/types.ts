export type BoneDimension = number | "auto" | `${number}%`;
export type BoneInset = number | "auto" | `${number}%`;
export type BoneColor = string;

export interface BoneLayout {
	width?: BoneDimension;
	height?: BoneDimension;
	minWidth?: BoneDimension;
	minHeight?: BoneDimension;
	maxWidth?: BoneDimension;
	maxHeight?: BoneDimension;
	flexGrow?: number;
	flexShrink?: number;
	flexBasis?: number | "auto";
	flexDirection?: "row" | "row-reverse" | "column" | "column-reverse";
	flexWrap?: "no-wrap" | "wrap" | "wrap-reverse";
	alignItems?:
		| "auto"
		| "flex-start"
		| "center"
		| "flex-end"
		| "stretch"
		| "baseline"
		| "space-between"
		| "space-around"
		| "space-evenly";
	alignSelf?:
		| "auto"
		| "flex-start"
		| "center"
		| "flex-end"
		| "stretch"
		| "baseline"
		| "space-between"
		| "space-around"
		| "space-evenly";
	justifyContent?: "flex-start" | "center" | "flex-end" | "space-between" | "space-around" | "space-evenly";
	position?: "static" | "relative" | "absolute";
	overflow?: "visible" | "hidden" | "scroll";
	top?: BoneInset;
	right?: BoneInset;
	bottom?: BoneInset;
	left?: BoneInset;
	margin?: BoneInset;
	marginX?: BoneInset;
	marginY?: BoneInset;
	marginTop?: BoneInset;
	marginRight?: BoneInset;
	marginBottom?: BoneInset;
	marginLeft?: BoneInset;
	padding?: number | `${number}%`;
	paddingX?: number | `${number}%`;
	paddingY?: number | `${number}%`;
	paddingTop?: number | `${number}%`;
	paddingRight?: number | `${number}%`;
	paddingBottom?: number | `${number}%`;
	paddingLeft?: number | `${number}%`;
	zIndex?: number;
}

export interface BoneNode {
	readonly id: string;
	visible: boolean;
	readonly destroyed: boolean;
	readonly effectivelyVisible: boolean;
	readonly screenX: number;
	readonly screenY: number;
	readonly width: number;
	readonly height: number;
	updateLayout(layout: BoneLayout): void;
	focus(): void;
	blur(): void;
	requestRender(): void;
	destroy(): void;
}

export interface BoneMouseEvent {
	readonly type: "down" | "up" | "move" | "drag" | "drag-end" | "drop" | "over" | "out" | "scroll";
	readonly x: number;
	readonly y: number;
	readonly button: number;
	readonly shift: boolean;
	readonly alt: boolean;
	readonly ctrl: boolean;
	readonly scrollDirection?: "up" | "down" | "left" | "right";
	readonly scrollDelta?: number;
	preventDefault(): void;
	stopPropagation(): void;
}

export interface BoneNodeOptions extends BoneLayout {
	onMouseDown?: (event: BoneMouseEvent) => void;
	onMouseDrag?: (event: BoneMouseEvent) => void;
	onMouseDragEnd?: (event: BoneMouseEvent) => void;
	onMouseOver?: (event: BoneMouseEvent) => void;
	onMouseOut?: (event: BoneMouseEvent) => void;
	onMouseScroll?: (event: BoneMouseEvent) => void;
}

export interface BoneBoxStyle {
	backgroundColor?: BoneColor;
	border?: boolean;
	borderStyle?: "single" | "double" | "rounded" | "heavy";
	borderColor?: BoneColor;
	title?: string;
	gap?: number | `${number}%`;
	rowGap?: number | `${number}%`;
	columnGap?: number | `${number}%`;
}

export interface BoneContainerNode extends BoneNode {
	append(child: BoneNode): void;
	remove(child: BoneNode): void;
	clear(): void;
	readonly childCount: number;
	updateStyle(style: BoneBoxStyle): void;
}

export interface BoneTextStyle {
	fg?: BoneColor;
	bg?: BoneColor;
	selectable?: boolean;
	wrapMode?: "none" | "char" | "word";
	truncate?: boolean;
	bold?: boolean;
	italic?: boolean;
	underline?: boolean;
	dim?: boolean;
	strikethrough?: boolean;
}

export interface BoneTextNode extends BoneNode {
	content: string;
	updateStyle(style: BoneTextStyle): void;
}

export interface BoneScrollViewNode extends BoneContainerNode {
	scrollTop: number;
	scrollLeft: number;
	readonly viewportHeight: number;
	readonly scrollHeight: number;
	readonly scrollWidth: number;
	scrollBy(delta: number | { x: number; y: number }): void;
	scrollTo(position: number | { x: number; y: number }): void;
}

export interface BoneMarkdownStyle {
	fg?: BoneColor;
	bg?: BoneColor;
	conceal?: boolean;
	concealCode?: boolean;
}

export interface BoneMarkdownNode extends BoneNode {
	content: string;
	streaming: boolean;
	updateStyle(style: BoneMarkdownStyle): void;
}

export interface BoneDiffStyle {
	fg?: BoneColor;
	view?: "unified" | "split";
	filetype?: string;
	wrapMode?: "none" | "char" | "word";
	showLineNumbers?: boolean;
	addedBg?: BoneColor;
	removedBg?: BoneColor;
	contextBg?: BoneColor;
	addedSignColor?: BoneColor;
	removedSignColor?: BoneColor;
}

export interface BoneDiffOptions extends BoneNodeOptions, BoneDiffStyle {
	id?: string;
	diff?: string;
}

export interface BoneDiffNode extends BoneNode {
	diff: string;
	updateStyle(style: BoneDiffStyle): void;
}

/** Raw RGBA pixels. Image decoding remains outside the renderer boundary. */
export interface BoneImageOptions extends BoneNodeOptions {
	id?: string;
	pixels: Uint8Array;
	pixelWidth: number;
	pixelHeight: number;
	terminalWidth: number;
	terminalHeight: number;
}

export interface BoneImageNode extends BoneNode {
	readonly pixelWidth: number;
	readonly pixelHeight: number;
	setPixels(pixels: Uint8Array, pixelWidth: number, pixelHeight: number): void;
}

export interface BoneTextareaStyle {
	textColor?: BoneColor;
	backgroundColor?: BoneColor;
	focusedTextColor?: BoneColor;
	focusedBackgroundColor?: BoneColor;
	placeholderColor?: BoneColor;
	wrapMode?: "none" | "char" | "word";
	showCursor?: boolean;
	cursorColor?: BoneColor;
}

export interface BoneTextareaNode extends BoneNode {
	value: string;
	placeholder: string | null;
	readonly cursorOffset: number;
	setCursorOffset(offset: number): void;
	insertText(text: string): void;
	updateStyle(style: BoneTextareaStyle): void;
}

export interface BoneBoxOptions extends BoneNodeOptions, BoneBoxStyle {
	id?: string;
	focusable?: boolean;
}

export interface BoneTextOptions extends BoneNodeOptions, BoneTextStyle {
	id?: string;
	content?: string;
}

export interface BoneScrollViewOptions extends BoneBoxOptions {
	scrollX?: boolean;
	scrollY?: boolean;
	stickyScroll?: boolean;
	stickyStart?: "bottom" | "top" | "left" | "right";
	viewportCulling?: boolean;
}

export interface BoneMarkdownOptions extends BoneNodeOptions, BoneMarkdownStyle {
	id?: string;
	content?: string;
	streaming?: boolean;
}

export interface BoneTextareaOptions extends BoneNodeOptions, BoneTextareaStyle {
	id?: string;
	initialValue?: string;
	placeholder?: string | null;
	onSubmit?: (value: string) => void;
	onChange?: (value: string) => void;
	onCancel?: () => void;
	keyBindings?: BoneKeyBinding<BoneInputAction>[];
	keyAliases?: Record<string, string>;
}

export type BoneInputAction =
	| "move-left"
	| "move-right"
	| "move-up"
	| "move-down"
	| "select-left"
	| "select-right"
	| "select-up"
	| "select-down"
	| "line-home"
	| "line-end"
	| "select-line-home"
	| "select-line-end"
	| "visual-line-home"
	| "visual-line-end"
	| "select-visual-line-home"
	| "select-visual-line-end"
	| "buffer-home"
	| "buffer-end"
	| "select-buffer-home"
	| "select-buffer-end"
	| "backspace"
	| "delete"
	| "delete-line"
	| "delete-to-line-end"
	| "delete-to-line-start"
	| "undo"
	| "redo"
	| "word-forward"
	| "word-backward"
	| "delete-word-forward"
	| "delete-word-backward"
	| "select-all"
	| "newline"
	| "submit"
	| "cancel";

export interface BoneKeyBinding<Action extends string> {
	name: string;
	action: Action;
	ctrl?: boolean;
	shift?: boolean;
	meta?: boolean;
	super?: boolean;
}

export interface BoneInputOptions extends BoneNodeOptions, BoneTextareaStyle {
	id?: string;
	value?: string;
	placeholder?: string;
	minLength?: number;
	maxLength?: number;
	keyBindings?: BoneKeyBinding<BoneInputAction>[];
	keyAliases?: Record<string, string>;
	onInput?: (value: string) => void;
	onChange?: (value: string) => void;
	onConfirm?: (value: string) => void;
	onCancel?: () => void;
}

export interface BoneInputNode extends BoneNode {
	value: string;
	placeholder: string;
	minLength: number;
	maxLength: number;
	readonly cursorOffset: number;
	setCursorOffset(offset: number): void;
	insertText(text: string): void;
	submit(): boolean;
	updateStyle(style: BoneTextareaStyle): void;
}

export interface BoneSelectItem<Value = string> {
	label: string;
	description?: string;
	value: Value;
}

export type BoneSelectAction = "move-up" | "move-down" | "move-up-fast" | "move-down-fast" | "confirm" | "cancel";

export interface BoneSelectStyle {
	backgroundColor?: BoneColor;
	textColor?: BoneColor;
	focusedBackgroundColor?: BoneColor;
	focusedTextColor?: BoneColor;
	selectedBackgroundColor?: BoneColor;
	selectedTextColor?: BoneColor;
	descriptionColor?: BoneColor;
	selectedDescriptionColor?: BoneColor;
	showScrollIndicator?: boolean;
	wrapSelection?: boolean;
	showDescription?: boolean;
	showSelectionIndicator?: boolean;
	itemSpacing?: number;
	fastScrollStep?: number;
}

export interface BoneSelectOptions<Value = string> extends BoneNodeOptions, BoneSelectStyle {
	id?: string;
	items?: BoneSelectItem<Value>[];
	selectedIndex?: number;
	keyBindings?: BoneKeyBinding<BoneSelectAction>[];
	keyAliases?: Record<string, string>;
	onChange?: (item: BoneSelectItem<Value>, index: number) => void;
	onConfirm?: (item: BoneSelectItem<Value>, index: number) => void;
	onCancel?: () => void;
}

export interface BoneSelectNode<Value = string> extends BoneNode {
	items: BoneSelectItem<Value>[];
	selectedIndex: number;
	readonly selectedItem: BoneSelectItem<Value> | null;
	moveUp(steps?: number): void;
	moveDown(steps?: number): void;
	confirm(): void;
	updateStyle(style: BoneSelectStyle): void;
}

export interface BoneSpacerOptions extends BoneNodeOptions {
	id?: string;
	size?: number;
	direction?: "horizontal" | "vertical";
}

export interface BoneRenderContext {
	readonly width: number;
	readonly height: number;
	createBox(options?: BoneBoxOptions): BoneContainerNode;
	createText(options?: BoneTextOptions): BoneTextNode;
	createScrollView(options?: BoneScrollViewOptions): BoneScrollViewNode;
	createMarkdown(options?: BoneMarkdownOptions): BoneMarkdownNode;
	createDiff(options?: BoneDiffOptions): BoneDiffNode;
	createImage(options: BoneImageOptions): BoneImageNode;
	createTextarea(options?: BoneTextareaOptions): BoneTextareaNode;
	createInput(options?: BoneInputOptions): BoneInputNode;
	createSelect<Value = string>(options?: BoneSelectOptions<Value>): BoneSelectNode<Value>;
	createSpacer(options?: BoneSpacerOptions): BoneNode;
	onResize(listener: BoneResizeListener): BoneUnsubscribe;
}

export interface BoneView {
	mount(context: BoneRenderContext): BoneNode;
}

export interface BoneKeyEvent {
	readonly name: string;
	readonly ctrl: boolean;
	readonly meta: boolean;
	readonly shift: boolean;
	readonly option: boolean;
	readonly sequence: string;
	readonly raw: string;
	readonly eventType: "press" | "repeat" | "release";
	readonly source: "raw" | "kitty";
	preventDefault(): void;
	stopPropagation(): void;
}

export type BoneKeyListener = (event: BoneKeyEvent) => void;
export type BoneUnsubscribe = () => void;
export type BoneResizeListener = (width: number, height: number) => void;
export type BoneOverlayAnchor =
	| "center"
	| "top-left"
	| "top-right"
	| "bottom-left"
	| "bottom-right"
	| "top-center"
	| "bottom-center"
	| "left-center"
	| "right-center";

export interface BoneOverlayOptions {
	anchor?: BoneOverlayAnchor;
	width?: BoneDimension;
	height?: BoneDimension;
	maxWidth?: BoneDimension;
	maxHeight?: BoneDimension;
	margin?: number;
	captureFocus?: boolean;
	zIndex?: number;
	backdropColor?: BoneColor;
}

export interface BoneOverlayHandle {
	readonly node: BoneNode;
	hide(): void;
	show(): void;
	focus(): void;
	update(options: BoneOverlayOptions): void;
	close(): void;
	readonly hidden: boolean;
}

export interface BoneRendererOptions {
	screenMode?: "alternate-screen" | "main-screen" | "split-footer";
	footerHeight?: number;
	useMouse?: boolean;
	useKittyKeyboard?: boolean;
	exitOnCtrlC?: boolean;
	clearOnShutdown?: boolean;
	targetFps?: number;
	backgroundColor?: BoneColor;
}

export interface BoneRenderer extends BoneRenderContext {
	readonly root: BoneContainerNode;
	readonly content: BoneContainerNode;
	readonly overlays: BoneContainerNode;
	readonly running: boolean;
	mount(view: BoneView, parent?: BoneContainerNode): BoneNode;
	showOverlay(node: BoneNode, options?: BoneOverlayOptions): BoneOverlayHandle;
	focus(node: BoneNode): void;
	blur(node: BoneNode): void;
	onKey(listener: BoneKeyListener): BoneUnsubscribe;
	requestRender(): void;
	resize(width: number, height: number): void;
	start(): void;
	stop(): void;
	destroy(): void;
	idle(): Promise<void>;
}

export interface BoneTestInput {
	typeText(text: string): Promise<void>;
	pressKey(key: string, modifiers?: { shift?: boolean; ctrl?: boolean; meta?: boolean }): void;
	pressEnter(modifiers?: { shift?: boolean; ctrl?: boolean; meta?: boolean }): void;
	pressEscape(modifiers?: { shift?: boolean; ctrl?: boolean; meta?: boolean }): void;
	pressArrow(
		direction: "up" | "down" | "left" | "right",
		modifiers?: { shift?: boolean; ctrl?: boolean; meta?: boolean },
	): void;
	paste(text: string): Promise<void>;
}

export interface BoneTestMouse {
	click(x: number, y: number, button?: "left" | "middle" | "right"): Promise<void>;
	drag(
		startX: number,
		startY: number,
		endX: number,
		endY: number,
		button?: "left" | "middle" | "right",
	): Promise<void>;
	scroll(x: number, y: number, direction: "up" | "down" | "left" | "right"): Promise<void>;
}

export interface BoneTestRenderer extends BoneRenderer {
	readonly input: BoneTestInput;
	readonly mouse: BoneTestMouse;
	flush(): Promise<void>;
	captureFrame(): string;
	captureCursor(): { x: number; y: number };
}
