import type { BoneContainerNode, BoneNode, BoneRenderContext, BoneTextNode, BoneView } from "@frelion/bone-tui";
import { type Theme, theme } from "../theme/theme.ts";

export interface OpenTUIDialogShellOptions {
	title: string;
	subtitle?: string;
	footer?: string;
	width?: number | `${number}%`;
	maxHeight?: number | `${number}%`;
	theme?: Theme;
}

export interface OpenTUIDialogMount {
	root: BoneContainerNode;
	body: BoneContainerNode;
	status: BoneTextNode;
}

/** Build a fixed-frame modal with a scrollable body and fixed footer/status. */
export function createOpenTUIDialogShell(
	context: BoneRenderContext,
	options: OpenTUIDialogShellOptions,
): OpenTUIDialogMount {
	const dialogTheme = options.theme ?? theme;
	const root = context.createBox({
		width: options.width ?? "72%",
		maxHeight: options.maxHeight ?? "88%",
		flexDirection: "column",
		padding: 1,
		border: true,
		borderStyle: "rounded",
		borderColor: dialogTheme.getFgColor("borderAccent"),
		backgroundColor: dialogTheme.getBgColor("customMessageBg"),
		focusable: true,
	});
	root.append(
		context.createText({ content: options.title, fg: dialogTheme.getFgColor("accent"), bold: true, height: 1 }),
	);
	if (options.subtitle) {
		root.append(
			context.createText({ content: options.subtitle, fg: dialogTheme.getFgColor("muted"), wrapMode: "word" }),
		);
	}
	root.append(context.createSpacer({ size: 1, direction: "vertical" }));
	const body = context.createBox({ width: "100%", flexDirection: "column", flexGrow: 1, minHeight: 1 });
	root.append(body);
	const status = context.createText({ content: "", fg: dialogTheme.getFgColor("warning"), wrapMode: "word" });
	status.visible = false;
	root.append(status);
	if (options.footer) {
		root.append(context.createSpacer({ size: 1, direction: "vertical" }));
		root.append(context.createText({ content: options.footer, fg: dialogTheme.getFgColor("dim"), wrapMode: "word" }));
	}
	return { root, body, status };
}

export interface OpenTUIDialogViewOptions extends OpenTUIDialogShellOptions {
	body: BoneView;
}

export class OpenTUIDialogViewV2 implements BoneView {
	private readonly options: OpenTUIDialogViewOptions;
	private mounted: OpenTUIDialogMount | undefined;

	constructor(options: OpenTUIDialogViewOptions) {
		this.options = options;
	}

	mount(context: BoneRenderContext): BoneNode {
		this.mounted = createOpenTUIDialogShell(context, this.options);
		this.mounted.body.append(this.options.body.mount(context));
		return this.mounted.root;
	}

	setStatus(message: string | undefined): void {
		if (!this.mounted) return;
		this.mounted.status.content = message ?? "";
		this.mounted.status.visible = Boolean(message);
	}
}
