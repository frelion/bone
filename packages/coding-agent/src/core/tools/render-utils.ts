import * as os from "node:os";
import type { ImageContent, TextContent } from "@frelion/bone-ai";
import type { BoneColor, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import { decodeOpenTUIImages, OpenTUIImageAttachments } from "../../modes/interactive/components/opentui-image.ts";
import type { Theme } from "../../modes/interactive/theme/theme.ts";
import { stripAnsi } from "../../utils/ansi.ts";
import { sanitizeBinaryOutput } from "../../utils/shell.ts";

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
	fg?: BoneColor;
	backgroundColor?: BoneColor;
	paddingX?: number;
	paddingY?: number;
	bold?: boolean;
}

export function structuredToolTextView(content: string, options: StructuredToolTextOptions = {}): BoneView {
	return {
		mount(context: BoneRenderContext): BoneNode {
			return context.createText({
				content: stripAnsi(content),
				fg: options.fg,
				bg: options.backgroundColor,
				paddingX: options.paddingX,
				paddingY: options.paddingY,
				bold: options.bold,
				wrapMode: "word",
			});
		},
	};
}

export function structuredToolResultView(
	result: { content: (TextContent | ImageContent)[] },
	text: string,
	options: StructuredToolTextOptions & { imageWidthCells?: number } = {},
): BoneView {
	return {
		mount(context: BoneRenderContext): BoneNode {
			const root = context.createBox({ flexDirection: "column" });
			if (stripAnsi(text).trim()) root.append(structuredToolTextView(text, options).mount(context));
			const imageContent = result.content.filter((part): part is ImageContent => part.type === "image");
			if (imageContent.length > 0) {
				const images = context.createBox({ flexDirection: "column" });
				images.append(
					context.createText({ content: "[decoding image...]", fg: options.fg, paddingX: options.paddingX }),
				);
				root.append(images);
				void decodeOpenTUIImages(imageContent, { terminalWidth: options.imageWidthCells ?? 40 }).then((decoded) => {
					images.clear();
					images.append(new OpenTUIImageAttachments(decoded).mount(context));
				});
			}
			return root;
		},
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
