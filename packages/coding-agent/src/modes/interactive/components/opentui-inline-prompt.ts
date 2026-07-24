import {
	BoxRenderable,
	type CliRenderer,
	InputRenderable,
	InputRenderableEvents,
	type KeyEvent,
	TextAttributes,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import type { ExtensionUIInputRequest } from "../../../core/extensions/ui-v2.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";

export interface OpenTUIInlinePromptRequest extends ExtensionUIInputRequest {
	secret?: boolean;
}

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Context-preserving command input mounted above the composer. */
export class OpenTUIInlinePrompt {
	readonly root: BoxRenderable;
	private readonly done: (value: string | undefined) => void;
	private readonly control: InputRenderable | TextareaRenderable;
	private readonly readValue: () => string;
	private readonly submitOnEnter: boolean;
	private completed = false;

	constructor(renderer: CliRenderer, request: OpenTUIInlinePromptRequest, done: (value: string | undefined) => void) {
		this.done = done;
		this.root = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "column",
			paddingX: 1,
			paddingY: 1,
			border: true,
			borderStyle: "rounded",
			borderColor: OPEN_TUI_COLORS.primary,
			backgroundColor: OPEN_TUI_COLORS.element,
		});
		this.root.add(
			new TextRenderable(renderer, {
				content: request.title,
				fg: OPEN_TUI_COLORS.primary,
				attributes: TextAttributes.BOLD,
			}),
		);
		if (request.multiline && !request.secret) {
			this.submitOnEnter = false;
			const textarea = new TextareaRenderable(renderer, {
				width: "100%",
				height: 5,
				maxHeight: 10,
				initialValue: request.initialValue ?? "",
				placeholder: request.placeholder ?? "",
				wrapMode: "word",
				textColor: OPEN_TUI_COLORS.text,
				focusedTextColor: OPEN_TUI_COLORS.text,
				placeholderColor: OPEN_TUI_COLORS.muted,
				cursorColor: OPEN_TUI_COLORS.primary,
				showCursor: true,
			});
			this.control = textarea;
			this.readValue = () => textarea.plainText;
		} else {
			this.submitOnEnter = true;
			let secretValue = request.secret ? (request.initialValue ?? "") : undefined;
			let visualValue = request.secret ? "*".repeat((secretValue ?? "").length) : (request.initialValue ?? "");
			const input = new InputRenderable(renderer, {
				width: "100%",
				value: visualValue,
				selectable: !request.secret,
				placeholder: request.secret ? "" : (request.placeholder ?? ""),
				backgroundColor: OPEN_TUI_COLORS.element,
				focusedBackgroundColor: OPEN_TUI_COLORS.element,
				textColor: OPEN_TUI_COLORS.text,
				focusedTextColor: OPEN_TUI_COLORS.text,
				placeholderColor: OPEN_TUI_COLORS.muted,
				cursorColor: OPEN_TUI_COLORS.primary,
				showCursor: true,
			});
			input.on(InputRenderableEvents.ENTER, () => this.finish(secretValue ?? input.value));
			this.control = input;
			this.readValue = () => secretValue ?? input.value;
			if (request.secret) {
				const mask = new TextRenderable(renderer, {
					content: `Secret input · ${(request.initialValue ?? "").length} characters`,
					fg: OPEN_TUI_COLORS.muted,
				});
				let applyingMask = false;
				input.on(InputRenderableEvents.INPUT, (rawValue: string) => {
					if (applyingMask || secretValue === undefined) return;
					const cursor = input.cursorOffset;
					if (/^\**$/.test(rawValue)) {
						const delta = rawValue.length - visualValue.length;
						if (delta < 0) {
							const start = Math.max(0, Math.min(cursor, secretValue.length));
							secretValue = `${secretValue.slice(0, start)}${secretValue.slice(start - delta)}`;
						} else if (delta > 0) {
							const start = Math.max(0, cursor - delta);
							secretValue = `${secretValue.slice(0, start)}${"*".repeat(delta)}${secretValue.slice(start)}`;
						}
					} else {
						let prefix = 0;
						while (
							prefix < visualValue.length &&
							prefix < rawValue.length &&
							visualValue[prefix] === rawValue[prefix]
						) {
							prefix++;
						}
						let suffix = 0;
						while (
							suffix < visualValue.length - prefix &&
							suffix < rawValue.length - prefix &&
							visualValue[visualValue.length - 1 - suffix] === rawValue[rawValue.length - 1 - suffix]
						) {
							suffix++;
						}
						const inserted = rawValue.slice(prefix, rawValue.length - suffix);
						secretValue = `${secretValue.slice(0, prefix)}${inserted}${secretValue.slice(secretValue.length - suffix)}`;
					}
					visualValue = "*".repeat(secretValue.length);
					applyingMask = true;
					input.value = visualValue;
					input.cursorOffset = Math.min(cursor, visualValue.length);
					applyingMask = false;
					mask.content = `Secret input · ${secretValue.length} characters`;
				});
				this.root.add(mask);
			}
		}
		this.root.add(this.control);
		this.root.add(
			new TextRenderable(renderer, {
				content: request.multiline && !request.secret ? "Ctrl+S submit · Esc cancel" : "Enter submit · Esc cancel",
				fg: OPEN_TUI_COLORS.dim,
				truncate: true,
			}),
		);
	}

	focus(): void {
		this.control.focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (matchesOpenTUIAction(event, "cancel")) {
			this.finish(undefined);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "save")) {
			this.finish(this.readValue());
			return consume(event);
		}
		if (this.submitOnEnter && matchesOpenTUIAction(event, "confirm")) {
			this.finish(this.readValue());
			return consume(event);
		}
		return false;
	}

	private finish(value: string | undefined): void {
		if (this.completed) return;
		this.completed = true;
		this.done(value);
	}
}
