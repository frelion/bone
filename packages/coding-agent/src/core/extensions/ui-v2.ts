import type { AgentToolResult } from "@frelion/bone-agent-core";
import type { BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import type { ReadonlyFooterDataProvider } from "../footer-data-provider.ts";

export type ExtensionUINotificationKind = "info" | "warning" | "error";

export interface ExtensionUIDialogOptionsV2 {
	signal?: AbortSignal;
	timeoutMs?: number;
}

export interface ExtensionUISelectOption<Value extends string = string> {
	value: Value;
	label: string;
	description?: string;
	disabled?: boolean;
}

export interface ExtensionUISelectRequest<Value extends string = string> extends ExtensionUIDialogOptionsV2 {
	title: string;
	options: readonly ExtensionUISelectOption<Value>[];
	initialValue?: Value;
}

export interface ExtensionUIConfirmRequest extends ExtensionUIDialogOptionsV2 {
	title: string;
	message: string;
	confirmLabel?: string;
	cancelLabel?: string;
}

export interface ExtensionUIInputRequest extends ExtensionUIDialogOptionsV2 {
	title: string;
	placeholder?: string;
	initialValue?: string;
	multiline?: boolean;
}

export interface ExtensionUIDialogService {
	select<Value extends string>(request: ExtensionUISelectRequest<Value>): Promise<Value | undefined>;
	confirm(request: ExtensionUIConfirmRequest): Promise<boolean>;
	input(request: ExtensionUIInputRequest): Promise<string | undefined>;
	notify(message: string, kind?: ExtensionUINotificationKind): void;
}

export type ExtensionUIViewFactory = () => BoneView;

export interface ExtensionUIViewHandle {
	readonly mounted: boolean;
	update(view: BoneView | ExtensionUIViewFactory): void;
	close(): void;
}

export type ExtensionUIWidgetPlacement = "aboveEditor" | "belowEditor";

export interface ExtensionUIWidgetOptionsV2 {
	placement?: ExtensionUIWidgetPlacement;
	order?: number;
}

export interface ExtensionUIWidgetService {
	set(
		key: string,
		view: BoneView | ExtensionUIViewFactory,
		options?: ExtensionUIWidgetOptionsV2,
	): ExtensionUIViewHandle;
	clear(key: string): void;
}

export interface ExtensionUIChromeService {
	setHeader(view: BoneView | ExtensionUIViewFactory | undefined): ExtensionUIViewHandle;
	setFooter(
		view: BoneView | ((footerData: ReadonlyFooterDataProvider) => BoneView) | undefined,
	): ExtensionUIViewHandle;
	setTitle(title: string): void;
}

export interface ExtensionUIEditorService {
	getText(): string;
	setText(text: string): void;
	insertText(text: string): void;
	open(request: ExtensionUIInputRequest): Promise<string | undefined>;
	setView(view: BoneView | ExtensionUIViewFactory | undefined): ExtensionUIViewHandle;
}

export interface ExtensionUIToolViewState<TState = unknown, TArgs = unknown> {
	toolCallId: string;
	args: TArgs;
	state: TState;
	cwd: string;
	executionStarted: boolean;
	argsComplete: boolean;
	isPartial: boolean;
	expanded: boolean;
	isError: boolean;
	previousView?: BoneView;
}

export interface ExtensionUIToolResultViewInput<TDetails = unknown> {
	result: AgentToolResult<TDetails>;
	isPartial: boolean;
	expanded: boolean;
}

export interface ExtensionUIToolViewRenderer<TArgs = unknown, TDetails = unknown, TState = unknown> {
	renderCall?(args: TArgs, context: ExtensionUIToolViewState<TState, TArgs>): BoneView;
	renderResult?(
		input: ExtensionUIToolResultViewInput<TDetails>,
		context: ExtensionUIToolViewState<TState, TArgs>,
	): BoneView;
}

export interface ExtensionUIToolResultService {
	setRenderer<TArgs = unknown, TDetails = unknown, TState = unknown>(
		toolName: string,
		renderer: ExtensionUIToolViewRenderer<TArgs, TDetails, TState> | undefined,
	): void;
}

export interface ExtensionUIAdvancedViewContext<Result> {
	done(result: Result): void;
	cancel(): void;
}

export interface ExtensionUIAdvancedOptions {
	presentation?: "dialog" | "overlay" | "replace";
	width?: number | `${number}%`;
	height?: number | `${number}%`;
}

/** Trusted escape hatch for extensions that need direct Bone view composition. */
export interface ExtensionUIAdvancedService {
	show<Result>(
		factory: (control: ExtensionUIAdvancedViewContext<Result>) => BoneView | Promise<BoneView>,
		options?: ExtensionUIAdvancedOptions,
	): Promise<Result | undefined>;
	close(): void;
	createView(factory: (context: BoneRenderContext) => BoneNode): BoneView;
}

export interface ExtensionUIV2Context {
	readonly version: 2;
	readonly available: boolean;
	readonly dialogs: ExtensionUIDialogService;
	readonly widgets: ExtensionUIWidgetService;
	readonly chrome: ExtensionUIChromeService;
	readonly editor: ExtensionUIEditorService;
	readonly toolResults: ExtensionUIToolResultService;
	readonly advanced: ExtensionUIAdvancedService;
}

function unavailableHandle(): ExtensionUIViewHandle {
	return { mounted: false, update: () => {}, close: () => {} };
}

/** Create the headless v2 UI used by print and JSON modes. */
export function createExtensionUIV2Context(): ExtensionUIV2Context {
	return {
		version: 2,
		available: false,
		dialogs: {
			select: async () => undefined,
			confirm: async () => false,
			input: async () => undefined,
			notify: () => {},
		},
		widgets: { set: () => unavailableHandle(), clear: () => {} },
		chrome: {
			setHeader: () => unavailableHandle(),
			setFooter: () => unavailableHandle(),
			setTitle: () => {},
		},
		editor: {
			getText: () => "",
			setText: () => {},
			insertText: () => {},
			open: async () => undefined,
			setView: () => unavailableHandle(),
		},
		toolResults: { setRenderer: () => {} },
		advanced: {
			show: async () => undefined,
			close: () => {},
			createView: (factory) => ({ mount: factory }),
		},
	};
}

export function resolveExtensionUIV2(context: { uiV2: ExtensionUIV2Context }): ExtensionUIV2Context {
	return context.uiV2;
}
