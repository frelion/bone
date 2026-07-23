import type { OverlayCloseReason, OverlayHandle, OverlayLayout, OverlayManager } from "@frelion/bone-tui";
import {
	type BoxRenderable,
	type CliRenderer,
	InputRenderable,
	InputRenderableEvents,
	type KeyEvent,
	type Renderable,
	TextareaRenderable,
} from "@opentui/core";
import type {
	ExtensionUIAdvancedOptions,
	ExtensionUIInputRequest,
	ExtensionUINotificationKind,
	ExtensionUIToolViewRenderer,
	ExtensionUIV2Context,
	ExtensionUIView,
	ExtensionUIViewHandle,
	ExtensionUIWidgetOptionsV2,
} from "../../core/extensions/ui-v2.ts";
import type { ReadonlyFooterDataProvider } from "../../core/footer-data-provider.ts";
import { createOpenTUIDialogShell } from "./components/opentui-dialog-v2.ts";
import { OpenTUISelectorViewV2 } from "./components/opentui-selector-v2.ts";
import { resolveOpenTUIDialogLayout } from "./opentui-design.ts";
import { matchesOpenTUIAction } from "./opentui-keymap.ts";
import { getThemeExportColors, theme } from "./theme/theme.ts";

export interface OpenTUIExtensionEditorAdapter {
	getText(): string;
	setText(text: string): void;
	insertText(text: string): void;
	focusTarget: Renderable | null;
}

export interface OpenTUIExtensionHostRegions {
	header: BoxRenderable;
	aboveEditor: BoxRenderable;
	editor: BoxRenderable;
	belowEditor: BoxRenderable;
	footer: BoxRenderable;
}

export interface OpenTUIExtensionHostOptions {
	renderer: CliRenderer;
	overlayManager: OverlayManager;
	regions: OpenTUIExtensionHostRegions;
	editor: OpenTUIExtensionEditorAdapter;
	footerData: ReadonlyFooterDataProvider;
	onNotify?: (message: string, kind: ExtensionUINotificationKind) => void;
	onTitle?: (title: string) => void;
}

interface WidgetRecord {
	readonly key: string;
	readonly sequence: number;
	placement: "aboveEditor" | "belowEditor";
	order: number;
	slot: NativeSlot;
}

interface NativeView {
	root: Renderable;
	focusTarget?: Renderable;
}

function resolveView(view: ExtensionUIView, renderer: CliRenderer): Renderable {
	return typeof view === "function" ? view(renderer) : view;
}

/** Pick the first native control inside a non-focusable extension root. */
function findFocusableDescendant(root: Renderable): Renderable | undefined {
	for (const child of root.getChildren()) {
		if (child.isDestroyed || !child.visible) continue;
		if (child.focusable) return child;
		const descendant = findFocusableDescendant(child);
		if (descendant) return descendant;
	}
	return undefined;
}

function dialogBackdropColor(): string {
	return getThemeExportColors().pageBg ?? theme.getBgColor("customMessageBg");
}

class NativeSlot implements ExtensionUIViewHandle {
	private readonly renderer: CliRenderer;
	private readonly parent: BoxRenderable;
	private node: Renderable | undefined;
	private closed = false;

	constructor(renderer: CliRenderer, parent: BoxRenderable, value: ExtensionUIView) {
		this.renderer = renderer;
		this.parent = parent;
		this.mount(value);
	}

	get mounted(): boolean {
		return this.node !== undefined && !this.node.isDestroyed && this.node.parent === this.parent;
	}

	update(view: ExtensionUIView): void {
		if (this.closed) return;
		this.unmount();
		this.mount(view);
	}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		this.unmount();
	}

	detach(): void {
		if (!this.node || this.node.isDestroyed || this.node.parent !== this.parent) return;
		this.parent.remove(this.node);
	}

	attach(): void {
		if (this.closed || !this.node || this.node.isDestroyed || this.node.parent === this.parent) return;
		if (this.node.parent) this.node.parent.remove(this.node);
		this.parent.add(this.node);
	}

	private mount(view: ExtensionUIView): void {
		const node = resolveView(view, this.renderer);
		if (node.isDestroyed) throw new Error("Cannot mount a destroyed OpenTUI renderable");
		if (node.parent) throw new Error("OpenTUI extension renderables must be detached before mounting");
		this.node = node;
		this.parent.add(node);
	}

	private unmount(): void {
		const node = this.node;
		this.node = undefined;
		if (node && !node.isDestroyed) node.destroyRecursively();
	}
}

class ExtensionInputView {
	private readonly request: ExtensionUIInputRequest;
	private readonly onSubmit: (value: string) => void;
	private readonly onCancel: () => void;
	private control: InputRenderable | TextareaRenderable | undefined;

	constructor(request: ExtensionUIInputRequest, onSubmit: (value: string) => void, onCancel: () => void) {
		this.request = request;
		this.onSubmit = onSubmit;
		this.onCancel = onCancel;
	}

	build(renderer: CliRenderer): NativeView {
		const dialog = createOpenTUIDialogShell(renderer, {
			title: this.request.title,
			footer: this.request.multiline ? "submit · newline · cancel" : "submit · cancel",
		});
		const onKeyDown = (event: KeyEvent) => {
			if (!matchesOpenTUIAction(event, "cancel")) return;
			event.preventDefault();
			event.stopPropagation();
			this.onCancel();
		};
		if (this.request.multiline) {
			let textarea: TextareaRenderable;
			textarea = new TextareaRenderable(renderer, {
				width: "100%",
				minHeight: 4,
				initialValue: this.request.initialValue,
				placeholder: this.request.placeholder,
				// Extension editors use the same fixed product contract as the
				// composer: Enter submits, Shift+Enter inserts a newline.
				keyBindings: [
					{ name: "return", action: "submit" },
					{ name: "return", shift: true, action: "newline" },
					{ name: "j", ctrl: true, action: "newline" },
				],
				onKeyDown,
				onSubmit: () => this.onSubmit(textarea.plainText),
			});
			this.control = textarea;
		} else {
			const input = new InputRenderable(renderer, {
				width: "100%",
				value: this.request.initialValue,
				placeholder: this.request.placeholder,
				onKeyDown,
			});
			input.on(InputRenderableEvents.ENTER, (value: string) => this.onSubmit(value));
			this.control = input;
		}
		dialog.body.add(this.control);
		return { root: dialog.root, focusTarget: this.control };
	}

	submit(): void {
		if (!this.control) return;
		if (this.control instanceof InputRenderable) this.control.submit();
		else this.control.submit();
	}

	focusTarget(): Renderable | undefined {
		return this.control;
	}
}

interface ActiveOverlayOperation {
	cancel(): void;
	handle?: OverlayHandle;
}

/** Native Extension UI host. Overlay/focus/lifecycle ownership lives in OverlayManager. */
export class OpenTUIExtensionHost {
	readonly context: ExtensionUIV2Context;
	private readonly options: OpenTUIExtensionHostOptions;
	private readonly widgets = new Map<string, WidgetRecord>();
	private readonly toolRenderers = new Map<string, ExtensionUIToolViewRenderer>();
	private readonly footerUnsubscribe: () => void;
	private header: NativeSlot | undefined;
	private footer: NativeSlot | undefined;
	private footerFactory: ((data: ReadonlyFooterDataProvider) => ExtensionUIView) | undefined;
	private editorView: NativeSlot | undefined;
	private activeOperation: ActiveOverlayOperation | undefined;
	private widgetSequence = 0;
	private disposed = false;

	constructor(options: OpenTUIExtensionHostOptions) {
		this.options = options;
		this.footerUnsubscribe = options.footerData.onBranchChange(() => this.refreshFooter());
		this.context = this.createContext();
	}

	getToolRenderer(toolName: string): ExtensionUIToolViewRenderer | undefined {
		return this.toolRenderers.get(toolName);
	}

	closeAdvanced(): void {
		this.activeOperation?.cancel();
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.activeOperation?.cancel();
		this.footerUnsubscribe();
		for (const widget of this.widgets.values()) widget.slot.close();
		this.widgets.clear();
		this.header?.close();
		this.header = undefined;
		this.footer?.close();
		this.footer = undefined;
		this.footerFactory = undefined;
		this.editorView?.close();
		this.editorView = undefined;
		this.toolRenderers.clear();
	}

	private createContext(): ExtensionUIV2Context {
		const host = this;
		return {
			version: 2,
			get available() {
				return !host.disposed;
			},
			dialogs: {
				select: (request) =>
					this.runDialog(request, (finish) => {
						const selectedIndex = request.initialValue
							? Math.max(
									0,
									request.options.findIndex((option) => option.value === request.initialValue),
								)
							: 0;
						const selector = new OpenTUISelectorViewV2({
							title: request.title,
							items: request.options,
							selectedIndex,
							onSelect: finish,
							onCancel: () => finish(undefined),
						});
						return {
							root: selector.build(this.options.renderer),
							focusTarget: selector.focusTarget,
							onKey: (event) => {
								if (!matchesOpenTUIAction(event, "cancel")) return false;
								finish(undefined);
								return true;
							},
						};
					}),
				confirm: async (request) => {
					const value = await this.runDialog<boolean>(request, (finish) => {
						const selector = new OpenTUISelectorViewV2({
							title: request.title,
							subtitle: request.message,
							items: [
								{ value: true, label: request.confirmLabel ?? "Confirm" },
								{ value: false, label: request.cancelLabel ?? "Cancel" },
							],
							onSelect: finish,
							onCancel: () => finish(false),
						});
						return {
							root: selector.build(this.options.renderer),
							focusTarget: selector.focusTarget,
							onKey: (event) => {
								if (!matchesOpenTUIAction(event, "cancel")) return false;
								finish(false);
								return true;
							},
						};
					});
					return value ?? false;
				},
				input: (request) => this.openInput(request),
				notify: (message, kind = "info") => {
					if (!this.disposed) this.options.onNotify?.(message, kind);
				},
			},
			widgets: {
				set: (key, view, widgetOptions) => this.setWidget(key, view, widgetOptions),
				clear: (key) => this.clearWidget(key),
			},
			chrome: {
				setHeader: (view) => this.setHeader(view),
				setFooter: (view) => this.setFooter(view),
				setTitle: (title) => {
					if (!this.disposed) this.options.onTitle?.(title);
				},
			},
			editor: {
				getText: () => (this.disposed ? "" : this.options.editor.getText()),
				setText: (text) => {
					if (!this.disposed) this.options.editor.setText(text);
				},
				insertText: (text) => {
					if (!this.disposed) this.options.editor.insertText(text);
				},
				open: (request) => this.openInput(request),
				setView: (view) => this.setEditorView(view),
			},
			toolResults: {
				setRenderer: (toolName, renderer) => {
					if (this.disposed) return;
					if (renderer) this.toolRenderers.set(toolName, renderer);
					else this.toolRenderers.delete(toolName);
				},
			},
			advanced: {
				show: (factory, advancedOptions) => this.showAdvanced(factory, advancedOptions),
				close: () => this.closeAdvanced(),
				createView: (factory) => factory,
			},
		};
	}

	private setWidget(
		key: string,
		view: ExtensionUIView,
		widgetOptions: ExtensionUIWidgetOptionsV2 = {},
	): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.clearWidget(key);
		const placement = widgetOptions.placement ?? "aboveEditor";
		const parent = placement === "aboveEditor" ? this.options.regions.aboveEditor : this.options.regions.belowEditor;
		const record: WidgetRecord = {
			key,
			sequence: this.widgetSequence++,
			placement,
			order: widgetOptions.order ?? 0,
			slot: new NativeSlot(this.options.renderer, parent, view),
		};
		this.widgets.set(key, record);
		this.sortWidgets(placement);
		return record.slot;
	}

	private clearWidget(key: string): void {
		const record = this.widgets.get(key);
		if (!record) return;
		record.slot.close();
		this.widgets.delete(key);
	}

	private sortWidgets(placement: "aboveEditor" | "belowEditor"): void {
		const parent = placement === "aboveEditor" ? this.options.regions.aboveEditor : this.options.regions.belowEditor;
		const records = [...this.widgets.values()]
			.filter((record) => record.placement === placement)
			.sort((left, right) => left.order - right.order || left.sequence - right.sequence);
		for (const record of records) record.slot.detach();
		for (const record of records) record.slot.attach();
		parent.requestRender();
	}

	private setHeader(view: ExtensionUIView | undefined): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.header?.close();
		this.header = view ? new NativeSlot(this.options.renderer, this.options.regions.header, view) : undefined;
		return this.header ?? this.unavailableHandle();
	}

	private setFooter(
		view: Renderable | ((footerData: ReadonlyFooterDataProvider) => ExtensionUIView) | undefined,
	): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.footer?.close();
		this.footerFactory = typeof view === "function" ? view : undefined;
		const resolved = typeof view === "function" ? view(this.options.footerData) : view;
		this.footer = resolved ? new NativeSlot(this.options.renderer, this.options.regions.footer, resolved) : undefined;
		return this.footer ?? this.unavailableHandle();
	}

	private refreshFooter(): void {
		if (this.disposed || !this.footerFactory || !this.footer?.mounted) return;
		this.footer.update(this.footerFactory(this.options.footerData));
	}

	private setEditorView(view: ExtensionUIView | undefined): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.editorView?.close();
		this.editorView = view ? new NativeSlot(this.options.renderer, this.options.regions.editor, view) : undefined;
		return this.editorView ?? this.unavailableHandle();
	}

	private openInput(request: ExtensionUIInputRequest): Promise<string | undefined> {
		return this.runDialog(request, (finish) => {
			const input = new ExtensionInputView(request, finish, () => finish(undefined));
			const view = input.build(this.options.renderer);
			return {
				...view,
				onKey: (event) => {
					if (!matchesOpenTUIAction(event, "cancel")) return false;
					finish(undefined);
					return true;
				},
			};
		});
	}

	private async runDialog<Result>(
		options: { signal?: AbortSignal; timeoutMs?: number },
		create: (finish: (value: Result | undefined) => void) => NativeView & {
			onKey?: (event: KeyEvent) => boolean;
		},
	): Promise<Result | undefined> {
		return this.runOverlay(options, async (finish) => create(finish));
	}

	private async runOverlay<Result>(
		options: { signal?: AbortSignal; timeoutMs?: number },
		create: (
			finish: (value: Result | undefined) => void,
		) =>
			| Promise<NativeView & { onKey?: (event: KeyEvent) => boolean }>
			| (NativeView & { onKey?: (event: KeyEvent) => boolean }),
		layout: (viewport: { width: number; height: number }) => OverlayLayout = (viewport) => ({
			...resolveOpenTUIDialogLayout(viewport.width, viewport.height),
			backdropColor: dialogBackdropColor(),
		}),
	): Promise<Result | undefined> {
		if (this.disposed) return undefined;
		this.activeOperation?.cancel();
		return new Promise<Result | undefined>((resolve) => {
			const localAbort = new AbortController();
			const signal = options.signal ? AbortSignal.any([options.signal, localAbort.signal]) : localAbort.signal;
			let resolved = false;
			let resultSet = false;
			let resultValue: Result | undefined;
			let handle: OverlayHandle | undefined;
			let routeKey: ((event: KeyEvent) => boolean) | undefined;
			const resolveOnce = (value: Result | undefined) => {
				if (resolved) return;
				resolved = true;
				if (this.activeOperation === operation) this.activeOperation = undefined;
				resolve(value);
			};
			const operation: ActiveOverlayOperation = {
				cancel: () => {
					if (resolved) return;
					localAbort.abort();
					if (handle) void handle.close("close");
					resolveOnce(undefined);
				},
			};
			this.activeOperation = operation;
			const finish = (value: Result | undefined) => {
				if (resultSet || resolved) return;
				resultSet = true;
				resultValue = value;
				if (handle) void handle.close("close");
				else localAbort.abort();
			};
			void this.options.overlayManager
				.openAsync(
					async () => {
						const created = await create(finish);
						routeKey = created.onKey;
						return { root: created.root, focusTarget: created.focusTarget ?? null };
					},
					{
						restoreFocus: this.options.editor.focusTarget,
						layout,
						signal,
						timeoutMs: options.timeoutMs,
						onKey: (event) => routeKey?.(event) ?? false,
						onClose: (_reason: OverlayCloseReason) => resolveOnce(resultSet ? resultValue : undefined),
					},
				)
				.then(
					(opened) => {
						handle = opened;
						operation.handle = opened;
						if (resultSet) void opened.close("close");
					},
					() => resolveOnce(resultSet ? resultValue : undefined),
				);
		});
	}

	private async showAdvanced<Result>(
		factory: (control: { done(result: Result): void; cancel(): void }) => ExtensionUIView | Promise<ExtensionUIView>,
		options: ExtensionUIAdvancedOptions = {},
	): Promise<Result | undefined> {
		return this.runOverlay(
			options,
			async (finish) => {
				const view = await factory({ done: finish, cancel: () => finish(undefined) });
				const root = resolveView(view, this.options.renderer);
				return {
					root,
					focusTarget: root.focusable ? root : findFocusableDescendant(root),
					onKey: (event) => {
						if (!matchesOpenTUIAction(event, "cancel")) return false;
						finish(undefined);
						return true;
					},
				};
			},
			(viewport) => {
				const responsive = resolveOpenTUIDialogLayout(viewport.width, viewport.height);
				return {
					width: options.width ?? (options.presentation === "overlay" ? "100%" : responsive.width),
					height: options.height ?? (options.presentation === "replace" ? "100%" : responsive.height),
					maxHeight: "100%",
					margin: options.presentation === "replace" ? 0 : responsive.margin,
					backdropColor: dialogBackdropColor(),
				};
			},
		);
	}

	private unavailableHandle(): ExtensionUIViewHandle {
		return { mounted: false, update: () => {}, close: () => {} };
	}
}
