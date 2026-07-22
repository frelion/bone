import type {
	BoneContainerNode,
	BoneInputNode,
	BoneNode,
	BoneOverlayHandle,
	BoneOverlayOptions,
	BoneRenderer,
	BoneTextareaNode,
	BoneView,
} from "@frelion/bone-tui";
import type {
	ExtensionUIAdvancedOptions,
	ExtensionUIInputRequest,
	ExtensionUINotificationKind,
	ExtensionUIToolViewRenderer,
	ExtensionUIV2Context,
	ExtensionUIViewFactory,
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
}

export interface OpenTUIExtensionHostRegions {
	header: BoneContainerNode;
	aboveEditor: BoneContainerNode;
	editor: BoneContainerNode;
	belowEditor: BoneContainerNode;
	footer: BoneContainerNode;
}

export interface OpenTUIExtensionHostOptions {
	renderer: BoneRenderer;
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
	slot: MountedSlot;
}

type DialogAction = (action: "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown") => void;

function resolveView(view: BoneView | ExtensionUIViewFactory): BoneView {
	return typeof view === "function" ? view() : view;
}

function dialogBackdropColor(): string {
	return getThemeExportColors().pageBg ?? theme.getBgColor("customMessageBg");
}

class MountedSlot implements ExtensionUIViewHandle {
	private readonly renderer: BoneRenderer;
	private readonly parent: BoneContainerNode;
	private value: BoneView | ExtensionUIViewFactory;
	private node: BoneNode | undefined;
	private closed = false;

	constructor(renderer: BoneRenderer, parent: BoneContainerNode, value: BoneView | ExtensionUIViewFactory) {
		this.renderer = renderer;
		this.parent = parent;
		this.value = value;
		this.mount();
	}

	get mounted(): boolean {
		return this.node !== undefined && !this.node.destroyed;
	}

	update(view: BoneView | ExtensionUIViewFactory): void {
		if (this.closed) return;
		this.value = view;
		this.unmount();
		this.mount();
		this.renderer.requestRender();
	}

	close(): void {
		if (this.closed) return;
		this.closed = true;
		this.unmount();
		this.renderer.requestRender();
	}

	detach(): void {
		if (this.closed) return;
		if (!this.node || this.node.destroyed) return;
		this.parent.remove(this.node);
	}

	attach(): void {
		if (this.closed) return;
		if (!this.node || this.node.destroyed) return;
		this.parent.append(this.node);
	}

	private mount(): void {
		this.node = this.renderer.mount(resolveView(this.value), this.parent);
	}

	private unmount(): void {
		if (!this.node) return;
		if (!this.node.destroyed) this.node.destroy();
		this.node = undefined;
	}
}

class ExtensionInputView implements BoneView {
	private readonly request: ExtensionUIInputRequest;
	private readonly onSubmit: (value: string) => void;
	private readonly onCancel: () => void;
	private control: BoneInputNode | BoneTextareaNode | undefined;

	constructor(request: ExtensionUIInputRequest, onSubmit: (value: string) => void, onCancel: () => void) {
		this.request = request;
		this.onSubmit = onSubmit;
		this.onCancel = onCancel;
	}

	mount(context: BoneRenderer): BoneNode {
		const dialog = createOpenTUIDialogShell(context, {
			title: this.request.title,
			footer: this.request.multiline
				? "Enter submit · Shift+Enter newline · Esc cancel"
				: "Enter submit · Esc cancel",
		});
		if (this.request.multiline) {
			this.control = context.createTextarea({
				width: "100%",
				minHeight: 4,
				initialValue: this.request.initialValue,
				placeholder: this.request.placeholder,
				onSubmit: this.onSubmit,
				onCancel: this.onCancel,
			});
		} else {
			this.control = context.createInput({
				width: "100%",
				value: this.request.initialValue,
				placeholder: this.request.placeholder,
				onConfirm: this.onSubmit,
				onCancel: this.onCancel,
			});
		}
		dialog.body.append(this.control);
		this.control.focus();
		return dialog.root;
	}

	submit(): void {
		if (!this.control) return;
		if ("submit" in this.control) this.control.submit();
		else this.onSubmit(this.control.value);
	}

	focus(): void {
		this.control?.focus();
	}
}

/** Native structured host for the breaking extension UI v2 contract. */
export class OpenTUIExtensionHost {
	readonly context: ExtensionUIV2Context;
	private readonly options: OpenTUIExtensionHostOptions;
	private readonly widgets = new Map<string, WidgetRecord>();
	private readonly toolRenderers = new Map<string, ExtensionUIToolViewRenderer>();
	private readonly unsubscribeKey: () => void;
	private readonly unsubscribeFooter: () => void;
	private readonly unsubscribeResize: () => void;
	private header: MountedSlot | undefined;
	private footer: MountedSlot | undefined;
	private footerFactory: ((data: ReadonlyFooterDataProvider) => BoneView) | undefined;
	private editorView: MountedSlot | undefined;
	private activeOverlay: BoneOverlayHandle | undefined;
	private activeOverlayLayout: ((width: number, height: number) => BoneOverlayOptions) | undefined;
	private activeDialogAction: DialogAction | undefined;
	private activeDialogCancel: (() => void) | undefined;
	private advancedCancel: (() => void) | undefined;
	private widgetSequence = 0;
	private disposed = false;

	constructor(options: OpenTUIExtensionHostOptions) {
		this.options = options;
		this.unsubscribeKey = options.renderer.onKey((event) => {
			if (!this.activeDialogAction) return;
			const actions = ["confirm", "cancel", "up", "down", "pageUp", "pageDown"] as const;
			for (const action of actions) {
				if (!matchesOpenTUIAction(event, action)) continue;
				event.preventDefault();
				event.stopPropagation();
				this.activeDialogAction(action);
				return;
			}
		});
		this.unsubscribeFooter = options.footerData.onBranchChange(() => this.refreshFooter());
		this.unsubscribeResize = options.renderer.onResize((width, height) => {
			const layout = this.activeOverlayLayout?.(width, height);
			if (layout) this.activeOverlay?.update(layout);
		});
		this.context = this.createContext();
	}

	getToolRenderer(toolName: string): ExtensionUIToolViewRenderer | undefined {
		return this.toolRenderers.get(toolName);
	}

	closeAdvanced(): void {
		this.advancedCancel?.();
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.activeDialogCancel?.();
		this.advancedCancel?.();
		this.unsubscribeKey();
		this.unsubscribeFooter();
		this.unsubscribeResize();
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
				select: async (request) =>
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
						return { view: selector, action: (action) => selector.handleAction(action) };
					}),
				confirm: async (request) =>
					(await this.runDialog(request, (finish) => {
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
						return { view: selector, action: (action) => selector.handleAction(action) };
					})) ?? false,
				input: async (request) => this.openInput(request),
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
				open: async (request) => this.openInput(request),
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
				createView: (factory) => ({ mount: factory }),
			},
		};
	}

	private setWidget(
		key: string,
		view: BoneView | ExtensionUIViewFactory,
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
			slot: new MountedSlot(this.options.renderer, parent, view),
		};
		this.widgets.set(key, record);
		this.sortWidgets(placement);
		return record.slot;
	}

	private clearWidget(key: string): void {
		if (this.disposed) return;
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

	private setHeader(view: BoneView | ExtensionUIViewFactory | undefined): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.header?.close();
		this.header = view ? new MountedSlot(this.options.renderer, this.options.regions.header, view) : undefined;
		return this.header ?? this.unavailableHandle();
	}

	private setFooter(
		view: BoneView | ((footerData: ReadonlyFooterDataProvider) => BoneView) | undefined,
	): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.footer?.close();
		this.footerFactory = typeof view === "function" ? view : undefined;
		const resolved = typeof view === "function" ? view(this.options.footerData) : view;
		this.footer = resolved
			? new MountedSlot(this.options.renderer, this.options.regions.footer, resolved)
			: undefined;
		return this.footer ?? this.unavailableHandle();
	}

	private refreshFooter(): void {
		if (this.disposed) return;
		if (!this.footerFactory || !this.footer?.mounted) return;
		this.footer?.update(this.footerFactory(this.options.footerData));
	}

	private setEditorView(view: BoneView | ExtensionUIViewFactory | undefined): ExtensionUIViewHandle {
		if (this.disposed) return this.unavailableHandle();
		this.editorView?.close();
		this.editorView = view ? new MountedSlot(this.options.renderer, this.options.regions.editor, view) : undefined;
		return this.editorView ?? this.unavailableHandle();
	}

	private openInput(request: ExtensionUIInputRequest): Promise<string | undefined> {
		return this.runDialog(request, (finish) => {
			const input = new ExtensionInputView(request, finish, () => finish(undefined));
			return {
				view: input,
				focus: () => input.focus(),
				action: (action) => {
					if (action === "cancel") finish(undefined);
					else if (action === "confirm") input.submit();
				},
			};
		});
	}

	private runDialog<Result>(
		options: { signal?: AbortSignal; timeoutMs?: number },
		create: (finish: (value: Result | undefined) => void) => {
			view: BoneView;
			action: DialogAction;
			focus?: () => void;
		},
	): Promise<Result | undefined> {
		if (this.disposed) return Promise.resolve(undefined);
		this.advancedCancel?.();
		this.activeDialogCancel?.();
		return new Promise((resolve) => {
			let settled = false;
			let timeout: ReturnType<typeof setTimeout> | undefined;
			const finish = (value: Result | undefined) => {
				if (settled) return;
				settled = true;
				if (timeout) clearTimeout(timeout);
				options.signal?.removeEventListener("abort", cancel);
				this.activeOverlay?.close();
				this.activeOverlay = undefined;
				this.activeOverlayLayout = undefined;
				this.activeDialogAction = undefined;
				this.activeDialogCancel = undefined;
				resolve(value);
			};
			const cancel = () => finish(undefined);
			if (options.signal?.aborted) {
				finish(undefined);
				return;
			}
			const dialog = create(finish);
			const node = dialog.view.mount(this.options.renderer);
			this.activeOverlayLayout = (width, height) => {
				const layout = resolveOpenTUIDialogLayout(width, height);
				return { ...layout, backdropColor: dialogBackdropColor() };
			};
			this.activeOverlay = this.options.renderer.showOverlay(
				node,
				this.activeOverlayLayout(this.options.renderer.width, this.options.renderer.height),
			);
			dialog.focus?.();
			this.activeDialogAction = dialog.action;
			this.activeDialogCancel = cancel;
			options.signal?.addEventListener("abort", cancel, { once: true });
			if (options.timeoutMs !== undefined && options.timeoutMs >= 0) timeout = setTimeout(cancel, options.timeoutMs);
		});
	}

	private async showAdvanced<Result>(
		factory: (control: { done(result: Result): void; cancel(): void }) => BoneView | Promise<BoneView>,
		options: ExtensionUIAdvancedOptions = {},
	): Promise<Result | undefined> {
		if (this.disposed) return undefined;
		this.activeDialogCancel?.();
		this.closeAdvanced();
		return new Promise((resolve) => {
			let settled = false;
			const finish = (value: Result | undefined) => {
				if (settled) return;
				settled = true;
				this.activeOverlay?.close();
				this.activeOverlay = undefined;
				this.activeOverlayLayout = undefined;
				this.advancedCancel = undefined;
				resolve(value);
			};
			this.advancedCancel = () => finish(undefined);
			void Promise.resolve(factory({ done: finish, cancel: () => finish(undefined) })).then(
				(view) => {
					if (settled) return;
					const node = view.mount(this.options.renderer);
					this.activeOverlayLayout = (width, height) => {
						const layout = resolveOpenTUIDialogLayout(width, height);
						return {
							width: options.width ?? (options.presentation === "overlay" ? "100%" : layout.width),
							height: options.height ?? (options.presentation === "replace" ? "100%" : layout.height),
							maxHeight: "100%",
							margin: options.presentation === "replace" ? 0 : layout.margin,
							backdropColor: dialogBackdropColor(),
						};
					};
					this.activeOverlay = this.options.renderer.showOverlay(
						node,
						this.activeOverlayLayout(this.options.renderer.width, this.options.renderer.height),
					);
				},
				() => finish(undefined),
			);
		});
	}

	private unavailableHandle(): ExtensionUIViewHandle {
		return { mounted: false, update: () => {}, close: () => {} };
	}
}
