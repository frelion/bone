import * as os from "node:os";
import type { ImageContent, TextContent } from "@frelion/bone-ai";
import { BoxRenderable, type CliRenderer, type ColorInput, createTextAttributes, TextRenderable } from "@opentui/core";
import { decodeOpenTUIImages, OpenTUIImageAttachments } from "../../modes/interactive/components/opentui-image.ts";
import type { Theme } from "../../modes/interactive/theme/theme.ts";
import { stripAnsi } from "../../utils/ansi.ts";
import { sanitizeBinaryOutput } from "../../utils/shell.ts";
import type { ExtensionUIViewFactory } from "../extensions/ui-v2.ts";

export function shortenPath(path: unknown): string {
	if (typeof path !== "string") return "";
	const home = os.homedir();
	if (path.startsWith(home)) {
		return `~${path.slice(home.length)}`;
	}
	return path;
}

export function linkPath(styledText: string, _rawPath: string, _cwd: string): string {
	return styledText;
}

export function str(value: unknown): string | null {
	if (typeof value === "string") return value;
	if (value == null) return "";
	return null;
}

export function replaceTabs(text: string): string {
	return text.replace(/\t/g, "   ");
}

export function normalizeDisplayText(text: string): string {
	return text.replace(/\r/g, "");
}

export function getTextOutput(
	result: { content: Array<{ type: string; text?: string; data?: string; mimeType?: string }> } | undefined,
	showImages: boolean,
): string {
	if (!result) return "";

	const textBlocks = result.content.filter((c) => c.type === "text");
	const imageBlocks = result.content.filter((c) => c.type === "image");

	let output = textBlocks.map((c) => sanitizeBinaryOutput(stripAnsi(c.text || "")).replace(/\r/g, "")).join("\n");

	if (imageBlocks.length > 0 && !showImages) {
		const imageIndicators = imageBlocks
			.map((img) => {
				const mimeType = img.mimeType ?? "image/unknown";
				return `[image: ${mimeType}; hidden]`;
			})
			.join("\n");
		output = output ? `${output}\n${imageIndicators}` : imageIndicators;
	}

	return output;
}

export interface StructuredToolTextOptions {
	fg?: ColorInput;
	backgroundColor?: ColorInput;
	paddingX?: number;
	paddingY?: number;
	bold?: boolean;
}

export function structuredToolTextView(
	content: string,
	options: StructuredToolTextOptions = {},
): ExtensionUIViewFactory {
	return (renderer: CliRenderer) =>
		new TextRenderable(renderer, {
			content: stripAnsi(content),
			fg: options.fg,
			bg: options.backgroundColor,
			paddingX: options.paddingX,
			paddingY: options.paddingY,
			attributes: options.bold ? createTextAttributes({ bold: true }) : undefined,
			wrapMode: "word",
		});
}

export function structuredToolResultView(
	result: { content: (TextContent | ImageContent)[] },
	text: string,
	options: StructuredToolTextOptions & { imageWidthCells?: number } = {},
): ExtensionUIViewFactory {
	return (renderer: CliRenderer) => {
		const root = new BoxRenderable(renderer, { flexDirection: "column" });
		if (stripAnsi(text).trim()) root.add(structuredToolTextView(text, options)(renderer));
		const imageContent = result.content.filter((part): part is ImageContent => part.type === "image");
		if (imageContent.length > 0) {
			const images = new BoxRenderable(renderer, { flexDirection: "column" });
			images.add(
				new TextRenderable(renderer, {
					content: "[decoding image...]",
					fg: options.fg,
					paddingX: options.paddingX,
				}),
			);
			root.add(images);
			void decodeOpenTUIImages(imageContent, { terminalWidth: options.imageWidthCells ?? 40 })
				.then((decoded) => {
					if (images.isDestroyed) return;
					for (const child of images.getChildren()) {
						images.remove(child);
						child.destroyRecursively();
					}
					images.add(new OpenTUIImageAttachments(renderer, decoded).root);
				})
				.catch((error: unknown) => {
					if (images.isDestroyed) return;
					for (const child of images.getChildren()) {
						images.remove(child);
						child.destroyRecursively();
					}
					images.add(
						new TextRenderable(renderer, {
							content: `[image: unable to decode; ${error instanceof Error ? error.message : String(error)}]`,
							fg: options.fg,
						}),
					);
				});
		}
		return root;
	};
}

export type ToolRenderResultLike<TDetails> = {
	content: (TextContent | ImageContent)[];
	details: TDetails;
};

export function invalidArgText(theme: Theme): string {
	return theme.fg("error", "[invalid arg]");
}

export function renderToolPath(
	rawPath: string | null,
	theme: Theme,
	cwd: string,
	options?: { emptyFallback?: string },
): string {
	if (rawPath === null) return invalidArgText(theme);
	const value = rawPath || options?.emptyFallback;
	if (!value) return theme.fg("toolOutput", "...");
	return linkPath(theme.fg("accent", shortenPath(value)), value, cwd);
}
