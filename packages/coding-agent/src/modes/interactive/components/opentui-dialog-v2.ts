import { BoxRenderable, type CliRenderer, createTextAttributes, type Renderable, TextRenderable } from "@opentui/core";
import { resolveOpenTUIDialogLayout } from "../opentui-design.ts";
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
	root: BoxRenderable;
	body: BoxRenderable;
	status: TextRenderable;
}

/** Build a native modal tree. The caller owns attach, focus and destruction. */
export function createOpenTUIDialogShell(
	renderer: CliRenderer,
	options: OpenTUIDialogShellOptions,
): OpenTUIDialogMount {
	const dialogTheme = options.theme ?? theme;
	const responsiveLayout = resolveOpenTUIDialogLayout(renderer.width, renderer.height);
	const root = new BoxRenderable(renderer, {
		width: options.width ?? responsiveLayout.width,
		height: responsiveLayout.height,
		maxHeight: options.maxHeight ?? responsiveLayout.maxHeight,
		flexDirection: "column",
		padding: 1,
		border: true,
		borderStyle: "single",
		borderColor: dialogTheme.getFgColor("borderAccent"),
		backgroundColor: dialogTheme.getBgColor("customMessageBg"),
	});
	root.add(
		new TextRenderable(renderer, {
			content: options.title,
			fg: dialogTheme.getFgColor("accent"),
			attributes: createTextAttributes({ bold: true }),
			height: 1,
		}),
	);
	if (options.subtitle) {
		root.add(
			new TextRenderable(renderer, {
				content: options.subtitle,
				fg: dialogTheme.getFgColor("muted"),
				wrapMode: "word",
			}),
		);
	}
	root.add(new BoxRenderable(renderer, { height: 1, flexShrink: 0 }));
	const body = new BoxRenderable(renderer, { width: "100%", flexDirection: "column", flexGrow: 1, minHeight: 1 });
	root.add(body);
	const status = new TextRenderable(renderer, {
		content: "",
		fg: dialogTheme.getFgColor("warning"),
		wrapMode: "word",
		visible: false,
	});
	root.add(status);
	return { root, body, status };
}

export interface OpenTUIDialogViewOptions extends OpenTUIDialogShellOptions {
	body: (renderer: CliRenderer) => Renderable;
}

/** Build a native dialog from a native body factory. */
export function createOpenTUIDialogView(renderer: CliRenderer, options: OpenTUIDialogViewOptions): OpenTUIDialogMount {
	const mounted = createOpenTUIDialogShell(renderer, options);
	mounted.body.add(options.body(renderer));
	return mounted;
}
