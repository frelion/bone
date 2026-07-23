import type { AuthInfoLink, OAuthDeviceCodeInfo } from "@frelion/bone-ai";
import {
	BoxRenderable,
	type CliRenderer,
	InputRenderable,
	InputRenderableEvents,
	type Renderable,
	TextRenderable,
} from "@opentui/core";
import { type Theme, theme } from "../theme/theme.ts";
import { createOpenTUIDialogShell, type OpenTUIDialogMount } from "./opentui-dialog-v2.ts";

export interface OpenTUILoginDialogOptionsV2 {
	providerId: string;
	providerName?: string;
	title?: string;
	onComplete: (success: boolean, message?: string) => void;
	onOpenUrl?: (url: string) => void;
	theme?: Theme;
}

interface PendingLoginInput {
	control: InputRenderable;
	resolve: (value: string) => void;
	reject: (error: Error) => void;
}

/** Structured native OAuth/device-code/login prompt flow. */
export class OpenTUILoginDialogV2 {
	private readonly options: OpenTUILoginDialogOptionsV2;
	private readonly loginTheme: Theme;
	private readonly abortController = new AbortController();
	private renderer: CliRenderer | undefined;
	private dialog: OpenTUIDialogMount | undefined;
	private content: BoxRenderable | undefined;
	private pendingInput: PendingLoginInput | undefined;
	private pendingLines: Array<{ text: string; tone: "text" | "accent" | "warning" | "muted" }> = [];
	private completed = false;

	constructor(options: OpenTUILoginDialogOptionsV2) {
		this.options = options;
		this.loginTheme = options.theme ?? theme;
	}

	get signal(): AbortSignal {
		return this.abortController.signal;
	}

	get root(): BoxRenderable | undefined {
		return this.dialog?.root;
	}

	get focusTarget(): Renderable | undefined {
		return this.pendingInput?.control;
	}

	build(renderer: CliRenderer): BoxRenderable {
		if (this.dialog) throw new Error("OpenTUILoginDialogV2 is already built");
		this.renderer = renderer;
		const providerName = this.options.providerName ?? this.options.providerId;
		this.dialog = createOpenTUIDialogShell(renderer, {
			title: this.options.title ?? `Login to ${providerName}`,
			footer: "submit · cancel",
			theme: this.loginTheme,
		});
		this.content = new BoxRenderable(renderer, { width: "100%", flexDirection: "column", gap: 1 });
		this.dialog.body.add(this.content);
		this.renderPendingLines();
		return this.dialog.root;
	}

	focus(): void {
		this.focusTarget?.focus();
	}

	showAuth(url: string, instructions?: string): void {
		this.pendingLines = [
			{ text: url, tone: "accent" },
			{ text: process.platform === "darwin" ? "Cmd+click to open" : "Ctrl+click to open", tone: "muted" },
		];
		if (instructions) this.pendingLines.push({ text: instructions, tone: "warning" });
		this.renderPendingLines();
		this.options.onOpenUrl?.(url);
	}

	showDeviceCode(info: OAuthDeviceCodeInfo): void {
		this.pendingLines = [
			{ text: info.verificationUri, tone: "accent" },
			{ text: `Enter code: ${info.userCode}`, tone: "warning" },
		];
		this.renderPendingLines();
	}

	showManualInput(prompt: string): Promise<string> {
		this.appendLine(prompt, "muted");
		return this.requestInput();
	}

	showPrompt(message: string, placeholder?: string): Promise<string> {
		this.appendLine(message, "text");
		if (placeholder) this.appendLine(`e.g., ${placeholder}`, "muted");
		return this.requestInput(placeholder);
	}

	showDetails(lines: readonly string[]): void {
		this.pendingLines = lines.map((text) => ({ text, tone: "text" as const }));
		this.renderPendingLines();
	}

	showInfo(message: string, links: readonly AuthInfoLink[] = [], showCloseHint = false): void {
		this.appendLine(message, "text");
		for (const link of links) this.appendLine(link.label ? `${link.label}: ${link.url}` : link.url, "accent");
		if (showCloseHint) this.appendLine("Esc close", "muted");
	}

	showWaiting(message: string): void {
		this.appendLine(message, "muted");
	}

	showProgress(message: string): void {
		this.appendLine(message, "muted");
	}

	handleAction(action: "confirm" | "cancel"): boolean {
		if (action === "cancel") {
			this.cancel();
			return true;
		}
		this.pendingInput?.control.submit();
		return true;
	}

	private requestInput(placeholder = ""): Promise<string> {
		const renderer = this.requireRenderer();
		const content = this.requireContent();
		this.rejectPendingInput(new Error("Login input was replaced"));
		const control = new InputRenderable(renderer, {
			width: "100%",
			placeholder,
			textColor: this.loginTheme.getFgColor("text"),
			focusedTextColor: this.loginTheme.getFgColor("text"),
			placeholderColor: this.loginTheme.getFgColor("dim"),
		});
		content.add(control);
		return new Promise((resolve, reject) => {
			this.pendingInput = { control, resolve, reject };
			control.once(InputRenderableEvents.ENTER, (value: string) => {
				if (this.pendingInput?.control !== control) return;
				this.pendingInput = undefined;
				resolve(value);
			});
		});
	}

	private appendLine(text: string, tone: "text" | "accent" | "warning" | "muted"): void {
		this.pendingLines.push({ text, tone });
		const renderer = this.renderer;
		const content = this.content;
		if (!renderer || !content) return;
		content.add(this.createLine(renderer, text, tone));
	}

	private renderPendingLines(): void {
		const renderer = this.renderer;
		const content = this.content;
		if (!renderer || !content) return;
		this.rejectPendingInput(new Error("Login view changed while input was pending"));
		for (const child of content.getChildren()) child.destroyRecursively();
		for (const line of this.pendingLines) content.add(this.createLine(renderer, line.text, line.tone));
	}

	private createLine(
		renderer: CliRenderer,
		text: string,
		tone: "text" | "accent" | "warning" | "muted",
	): TextRenderable {
		return new TextRenderable(renderer, {
			content: text,
			fg: this.loginTheme.getFgColor(tone),
			wrapMode: "word",
			selectable: true,
		});
	}

	private cancel(): void {
		if (this.completed) return;
		this.completed = true;
		this.abortController.abort();
		this.rejectPendingInput(new Error("Login cancelled"));
		this.options.onComplete(false, "Login cancelled");
	}

	private rejectPendingInput(error: Error): void {
		const pending = this.pendingInput;
		if (!pending) return;
		this.pendingInput = undefined;
		pending.reject(error);
		if (!pending.control.isDestroyed) pending.control.destroyRecursively();
	}

	private requireRenderer(): CliRenderer {
		if (!this.renderer) throw new Error("OpenTUILoginDialogV2 must be built before requesting input");
		return this.renderer;
	}

	private requireContent(): BoxRenderable {
		if (!this.content) throw new Error("OpenTUILoginDialogV2 must be built before requesting input");
		return this.content;
	}
}
