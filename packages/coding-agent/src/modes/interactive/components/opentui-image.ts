import type { ImageContent } from "@frelion/bone-ai";
import { BoxRenderable, type CliRenderer, FrameBufferRenderable, TextRenderable } from "@opentui/core";
import { applyExifOrientation } from "../../../utils/exif-orientation.ts";
import { loadPhoton } from "../../../utils/photon.ts";
import { type Theme, theme } from "../theme/theme.ts";
import type { OpenTUIImageAttachment } from "./opentui-rich-messages.ts";

declare const Bun: {
	FFI: {
		ptr(value: ArrayBufferView): number;
	};
};

export interface OpenTUIImageDecodeOptions {
	terminalWidth?: number;
	maximumBytes?: number;
	maximumPixels?: number;
}

const DEFAULT_TERMINAL_WIDTH = 40;
const DEFAULT_MAXIMUM_BYTES = 20 * 1024 * 1024;
const DEFAULT_MAXIMUM_PIXELS = 16_000_000;

/** Decode model image content into the renderer's RGBA boundary. */
export async function decodeOpenTUIImage(
	content: ImageContent,
	options: OpenTUIImageDecodeOptions = {},
): Promise<OpenTUIImageAttachment> {
	const fallback = (error: string): OpenTUIImageAttachment => ({ mimeType: content.mimeType, error });
	let bytes: Uint8Array;
	try {
		bytes = Buffer.from(content.data, "base64");
	} catch {
		return fallback("invalid base64 data");
	}
	if (bytes.length === 0) return fallback("empty image data");
	if (bytes.length > (options.maximumBytes ?? DEFAULT_MAXIMUM_BYTES)) return fallback("image exceeds decode limit");

	let photon: Awaited<ReturnType<typeof loadPhoton>>;
	try {
		photon = await loadPhoton();
	} catch {
		return fallback("image decoder unavailable");
	}
	if (!photon) return fallback("image decoder unavailable");
	let image: ReturnType<typeof photon.PhotonImage.new_from_byteslice> | undefined;
	try {
		const rawImage = photon.PhotonImage.new_from_byteslice(bytes);
		image = applyExifOrientation(photon, rawImage, bytes);
		if (image !== rawImage) rawImage.free();
		const pixelWidth = image.get_width();
		const pixelHeight = image.get_height();
		if (pixelWidth <= 0 || pixelHeight <= 0) return fallback("image has invalid dimensions");
		if (pixelWidth * pixelHeight > (options.maximumPixels ?? DEFAULT_MAXIMUM_PIXELS)) {
			return fallback("image dimensions exceed decode limit");
		}
		const terminalWidth = Math.max(1, options.terminalWidth ?? DEFAULT_TERMINAL_WIDTH);
		const terminalHeight = Math.max(1, Math.ceil((pixelHeight / pixelWidth) * (terminalWidth / 2)));
		return {
			mimeType: content.mimeType,
			pixels: new Uint8Array(image.get_raw_pixels()),
			pixelWidth,
			pixelHeight,
			terminalWidth,
			terminalHeight,
		};
	} catch {
		return fallback("unsupported or corrupt image data");
	} finally {
		image?.free();
	}
}

export async function decodeOpenTUIImages(
	content: readonly (ImageContent | { type: string })[],
	options?: OpenTUIImageDecodeOptions,
): Promise<OpenTUIImageAttachment[]> {
	const images = content.filter((part): part is ImageContent => part.type === "image");
	return Promise.all(images.map((part) => decodeOpenTUIImage(part, options)));
}

export class OpenTUIRgbaImage extends FrameBufferRenderable {
	private pixels: Uint8Array;
	private pixelWidth: number;
	private pixelHeight: number;

	constructor(
		renderer: CliRenderer,
		options: {
			pixels: Uint8Array;
			pixelWidth: number;
			pixelHeight: number;
			terminalWidth: number;
			terminalHeight: number;
		},
	) {
		super(renderer, { width: options.terminalWidth, height: options.terminalHeight });
		this.pixels = options.pixels;
		this.pixelWidth = options.pixelWidth;
		this.pixelHeight = options.pixelHeight;
		this.redraw();
	}

	private redraw(): void {
		const expectedLength = this.pixelWidth * this.pixelHeight * 4;
		if (this.pixelWidth <= 0 || this.pixelHeight <= 0 || this.pixels.length !== expectedLength) {
			throw new RangeError(`Expected ${expectedLength} RGBA bytes, received ${this.pixels.length}`);
		}
		this.frameBuffer.clear();
		this.frameBuffer.drawSuperSampleBuffer(
			0,
			0,
			Bun.FFI.ptr(this.pixels),
			this.pixels.length,
			"rgba8unorm",
			this.pixelWidth * 4,
		);
	}
}

export class OpenTUIImageAttachments {
	readonly root: BoxRenderable;
	private readonly attachments: readonly OpenTUIImageAttachment[];
	private readonly viewTheme: Theme;

	constructor(renderer: CliRenderer, attachments: readonly OpenTUIImageAttachment[], viewTheme: Theme = theme) {
		this.attachments = attachments;
		this.viewTheme = viewTheme;
		this.root = new BoxRenderable(renderer, { flexDirection: "column", paddingX: 1 });
		for (const attachment of this.attachments) {
			if (
				attachment.pixels &&
				attachment.pixelWidth &&
				attachment.pixelHeight &&
				attachment.terminalWidth &&
				attachment.terminalHeight
			) {
				this.root.add(
					new OpenTUIRgbaImage(renderer, {
						pixels: attachment.pixels,
						pixelWidth: attachment.pixelWidth,
						pixelHeight: attachment.pixelHeight,
						terminalWidth: attachment.terminalWidth,
						terminalHeight: attachment.terminalHeight,
					}),
				);
			} else {
				this.root.add(
					new TextRenderable(renderer, {
						content: `[image: ${attachment.mimeType}; ${attachment.error ?? "unable to decode"}]`,
						fg: this.viewTheme.getFgColor("warning"),
						wrapMode: "word",
					}),
				);
			}
		}
	}
}
