import type { AuthInfoLink, OAuthDeviceCodeInfo } from "@frelion/bone-ai";
import type { BoneContainerNode, BoneInputNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
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

/** Structured OAuth/device-code/login prompt flow. */
export class OpenTUILoginDialogV2 implements BoneView {
	private readonly options: OpenTUILoginDialogOptionsV2;
	private readonly loginTheme: Theme;
	private readonly abortController = new AbortController();
	private context: BoneRenderContext | undefined;
	private dialog: OpenTUIDialogMount | undefined;
	private content: BoneContainerNode | undefined;
	private input: BoneInputNode | undefined;
	private inputResolver: ((value: string) => void) | undefined;
	private inputRejecter: ((error: Error) => void) | undefined;
	private pendingLines: Array<{ text: string; tone: "text" | "accent" | "warning" | "muted" }> = [];

	constructor(options: OpenTUILoginDialogOptionsV2) {
		this.options = options;
		this.loginTheme = options.theme ?? theme;
	}

	get signal(): AbortSignal {
		return this.abortController.signal;
	}

	mount(context: BoneRenderContext): BoneNode {
		this.context = context;
		const providerName = this.options.providerName ?? this.options.providerId;
		this.dialog = createOpenTUIDialogShell(context, {
			title: this.options.title ?? `Login to ${providerName}`,
			footer: "Enter submit · Esc cancel",
			theme: this.loginTheme,
		});
		this.content = context.createBox({ width: "100%", flexDirection: "column", gap: 1 });
		this.dialog.body.append(this.content);
		this.renderPendingLines();
		return this.dialog.root;
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
		this.input?.submit();
		return true;
	}

	private requestInput(placeholder = ""): Promise<string> {
		const context = this.requireContext();
		const content = this.requireContent();
		this.input = context.createInput({
			width: "100%",
			placeholder,
			textColor: this.loginTheme.getFgColor("text"),
			focusedTextColor: this.loginTheme.getFgColor("text"),
			placeholderColor: this.loginTheme.getFgColor("dim"),
			onConfirm: (value) => {
				this.inputResolver?.(value);
				this.inputResolver = undefined;
				this.inputRejecter = undefined;
			},
			onCancel: () => this.cancel(),
		});
		content.append(this.input);
		this.input.focus();
		return new Promise((resolve, reject) => {
			this.inputResolver = resolve;
			this.inputRejecter = reject;
		});
	}

	private appendLine(text: string, tone: "text" | "accent" | "warning" | "muted"): void {
		this.pendingLines.push({ text, tone });
		const context = this.context;
		const content = this.content;
		if (!context || !content) return;
		content.append(
			context.createText({
				content: text,
				fg: this.loginTheme.getFgColor(tone),
				wrapMode: "word",
				selectable: true,
			}),
		);
	}

	private renderPendingLines(): void {
		const context = this.context;
		const content = this.content;
		if (!context || !content) return;
		content.clear();
		this.input = undefined;
		for (const line of this.pendingLines) {
			content.append(
				context.createText({
					content: line.text,
					fg: this.loginTheme.getFgColor(line.tone),
					wrapMode: "word",
					selectable: true,
				}),
			);
		}
	}

	private cancel(): void {
		this.abortController.abort();
		this.inputRejecter?.(new Error("Login cancelled"));
		this.inputResolver = undefined;
		this.inputRejecter = undefined;
		this.options.onComplete(false, "Login cancelled");
	}

	private requireContext(): BoneRenderContext {
		if (!this.context) throw new Error("OpenTUILoginDialogV2 must be mounted before requesting input");
		return this.context;
	}

	private requireContent(): BoneContainerNode {
		if (!this.content) throw new Error("OpenTUILoginDialogV2 must be mounted before requesting input");
		return this.content;
	}
}
