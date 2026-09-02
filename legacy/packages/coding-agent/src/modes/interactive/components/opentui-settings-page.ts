import {
	BoxRenderable,
	type CliRenderer,
	InputRenderable,
	type KeyEvent,
	SelectRenderable,
	SelectRenderableEvents,
	TextAttributes,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import type {
	ExtensionUIConfirmRequest,
	ExtensionUIDialogService,
	ExtensionUIInputRequest,
	ExtensionUINotificationKind,
	ExtensionUISelectRequest,
} from "../../../core/extensions/ui-v2.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";
import { OpenTUISettingsSaveRequested } from "./opentui-settings-center.ts";

type ActiveControl = SelectRenderable | InputRenderable | TextareaRenderable;

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Main-area host for the transactional settings workflow. */
export class OpenTUISettingsPage {
	readonly root: BoxRenderable;
	readonly dialogs: ExtensionUIDialogService;
	private readonly renderer: CliRenderer;
	private readonly body: BoxRenderable;
	private readonly confirm: (request: ExtensionUIConfirmRequest) => Promise<boolean>;
	private readonly notify: (message: string, kind?: ExtensionUINotificationKind) => void;
	private activeControl: ActiveControl | undefined;
	private activeKeyHandler: ((event: KeyEvent) => boolean) | undefined;
	private cancelActive: (() => void) | undefined;
	private saveAfterCurrent = false;
	private disposed = false;

	constructor(
		renderer: CliRenderer,
		options: {
			confirm: (request: ExtensionUIConfirmRequest) => Promise<boolean>;
			notify: (message: string, kind?: ExtensionUINotificationKind) => void;
		},
	) {
		this.renderer = renderer;
		this.confirm = options.confirm;
		this.notify = options.notify;
		this.root = new BoxRenderable(renderer, {
			width: "100%",
			height: "100%",
			minHeight: 0,
			flexDirection: "column",
			backgroundColor: OPEN_TUI_COLORS.page,
		});
		this.body = new BoxRenderable(renderer, {
			width: "100%",
			flexGrow: 1,
			minHeight: 0,
			flexDirection: "column",
			paddingX: 1,
			paddingY: 1,
		});
		this.root.add(this.body);
		this.dialogs = {
			select: (request) => this.select(request),
			input: (request) => this.input(request),
			confirm: (request) => this.confirm(request),
			notify: (message, kind) => this.notify(message, kind),
		};
	}

	focus(): void {
		this.activeControl?.focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		return this.activeKeyHandler?.(event) ?? false;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.cancelActive?.();
		this.cancelActive = undefined;
		this.activeControl = undefined;
		this.activeKeyHandler = undefined;
	}

	private async select<Value extends string>(request: ExtensionUISelectRequest<Value>): Promise<Value | undefined> {
		if (this.disposed) return undefined;
		if (this.saveAfterCurrent) {
			this.saveAfterCurrent = false;
			throw new OpenTUISettingsSaveRequested();
		}
		return await new Promise<Value | undefined>((resolve) => {
			let settled = false;
			const finish = (value: Value | undefined) => {
				if (settled) return;
				settled = true;
				this.clearActive();
				resolve(value);
			};
			this.renderHeader(request.title);
			const selectedIndex = Math.max(
				0,
				request.initialValue === undefined
					? 0
					: request.options.findIndex((option) => option.value === request.initialValue),
			);
			const select = new SelectRenderable(this.renderer, {
				width: "100%",
				flexGrow: 1,
				minHeight: 1,
				options: request.options.map((option) => ({
					name: option.disabled ? `${option.label} (unavailable)` : option.label,
					description: option.description ?? "",
				})),
				selectedIndex,
				showDescription: true,
				showSelectionIndicator: true,
				wrapSelection: true,
				backgroundColor: OPEN_TUI_COLORS.page,
				textColor: OPEN_TUI_COLORS.text,
				focusedTextColor: OPEN_TUI_COLORS.text,
				selectedBackgroundColor: OPEN_TUI_COLORS.selection,
				selectedTextColor: OPEN_TUI_COLORS.selectionText,
				descriptionColor: OPEN_TUI_COLORS.muted,
				selectedDescriptionColor: OPEN_TUI_COLORS.muted,
			});
			const choose = () => {
				const option = request.options[select.getSelectedIndex()];
				if (option && !option.disabled) finish(option.value);
			};
			select.on(SelectRenderableEvents.ITEM_SELECTED, choose);
			this.body.add(select);
			this.renderFooter("Enter open · Esc back or close · Ctrl+S save");
			this.activeControl = select;
			this.cancelActive = () => finish(undefined);
			this.activeKeyHandler = (event) => {
				if (matchesOpenTUIAction(event, "cancel")) {
					finish(undefined);
					return consume(event);
				}
				if (matchesOpenTUIAction(event, "save")) {
					const shortcut = request.shortcuts?.find((item) => item.action === "save");
					if (shortcut) finish(shortcut.value);
					return consume(event);
				}
				if (matchesOpenTUIAction(event, "up")) {
					select.moveUp();
					return consume(event);
				}
				if (matchesOpenTUIAction(event, "down")) {
					select.moveDown();
					return consume(event);
				}
				if (matchesOpenTUIAction(event, "confirm")) {
					choose();
					return consume(event);
				}
				return false;
			};
			this.watchAbort(request.signal, () => finish(undefined));
			select.focus();
			this.renderer.requestRender();
		});
	}

	private async input(request: ExtensionUIInputRequest): Promise<string | undefined> {
		if (this.disposed) return undefined;
		return await new Promise<string | undefined>((resolve) => {
			let settled = false;
			this.renderHeader(request.title);
			const finish = (value: string | undefined, save = false) => {
				if (settled) return;
				settled = true;
				this.saveAfterCurrent = save;
				this.clearActive();
				resolve(value);
			};
			let control: InputRenderable | TextareaRenderable;
			let readValue: () => string;
			if (request.multiline) {
				const textarea = new TextareaRenderable(this.renderer, {
					width: "100%",
					flexGrow: 1,
					minHeight: 4,
					initialValue: request.initialValue ?? "",
					placeholder: request.placeholder ?? "",
					wrapMode: "word",
					textColor: OPEN_TUI_COLORS.text,
					focusedTextColor: OPEN_TUI_COLORS.text,
					placeholderColor: OPEN_TUI_COLORS.muted,
					cursorColor: OPEN_TUI_COLORS.primary,
					showCursor: true,
				});
				control = textarea;
				readValue = () => textarea.plainText;
			} else {
				const input = new InputRenderable(this.renderer, {
					width: "100%",
					value: request.initialValue ?? "",
					placeholder: request.placeholder ?? "",
					textColor: OPEN_TUI_COLORS.text,
					focusedTextColor: OPEN_TUI_COLORS.text,
					placeholderColor: OPEN_TUI_COLORS.muted,
				});
				input.on("enter", () => finish(readValue()));
				control = input;
				readValue = () => input.value;
			}
			this.body.add(control);
			this.renderFooter(
				request.multiline ? "Ctrl+S save settings · Esc back" : "Enter apply · Ctrl+S apply and save · Esc back",
			);
			this.activeControl = control;
			this.cancelActive = () => finish(undefined);
			this.activeKeyHandler = (event) => {
				if (matchesOpenTUIAction(event, "cancel")) {
					finish(undefined);
					return consume(event);
				}
				if (matchesOpenTUIAction(event, "save")) {
					finish(readValue(), true);
					return consume(event);
				}
				if (!request.multiline && matchesOpenTUIAction(event, "confirm")) {
					finish(readValue());
					return consume(event);
				}
				return false;
			};
			this.watchAbort(request.signal, () => finish(undefined));
			control.focus();
			this.renderer.requestRender();
		});
	}

	private renderHeader(title: string): void {
		this.clearBody();
		this.body.add(
			new TextRenderable(this.renderer, {
				content: "Settings",
				fg: OPEN_TUI_COLORS.primary,
				attributes: TextAttributes.BOLD,
			}),
		);
		this.body.add(
			new TextRenderable(this.renderer, {
				content: title,
				fg: OPEN_TUI_COLORS.text,
				attributes: TextAttributes.BOLD,
			}),
		);
		this.body.add(new BoxRenderable(this.renderer, { height: 1, flexShrink: 0 }));
	}

	private renderFooter(content: string): void {
		this.body.add(new BoxRenderable(this.renderer, { height: 1, flexShrink: 0 }));
		this.body.add(new TextRenderable(this.renderer, { content, fg: OPEN_TUI_COLORS.dim, truncate: true }));
	}

	private watchAbort(signal: AbortSignal | undefined, cancel: () => void): void {
		if (signal?.aborted) cancel();
		else signal?.addEventListener("abort", cancel, { once: true });
	}

	private clearActive(): void {
		this.activeControl = undefined;
		this.activeKeyHandler = undefined;
		this.cancelActive = undefined;
	}

	private clearBody(): void {
		for (const child of this.body.getChildren()) child.destroyRecursively();
	}
}
