import { getTreeSitterClient } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { BoneRendererImpl } from "./renderer.ts";
import type { BoneRendererOptions, BoneTestInput, BoneTestRenderer } from "./types.ts";

let activeTestRenderers = 0;
let testRendererTeardown: Promise<void> = Promise.resolve();

class BoneTestRendererImpl extends BoneRendererImpl implements BoneTestRenderer {
	readonly input: BoneTestInput;
	readonly mouse: BoneTestRenderer["mouse"];
	private readonly testSetup: TestRendererSetup;
	private destroyed = false;

	constructor(setup: TestRendererSetup) {
		super(setup.renderer);
		activeTestRenderers++;
		this.testSetup = setup;
		this.input = {
			typeText: (text) => setup.mockInput.typeText(text),
			pressKey: (key, modifiers) => setup.mockInput.pressKey(key, modifiers),
			pressEnter: (modifiers) => setup.mockInput.pressEnter(modifiers),
			pressEscape: (modifiers) => setup.mockInput.pressEscape(modifiers),
			pressArrow: (direction, modifiers) => setup.mockInput.pressArrow(direction, modifiers),
			paste: (text) => setup.mockInput.pasteBracketedText(text),
		};
		this.mouse = {
			click: (x, y, button = "left") => {
				const nativeButton = button === "left" ? 0 : button === "middle" ? 1 : 2;
				return setup.mockMouse.click(x, y, nativeButton);
			},
			scroll: (x, y, direction) => setup.mockMouse.scroll(x, y, direction),
		};
	}

	async flush(): Promise<void> {
		await this.testSetup.flush();
		await this.testSetup.renderOnce();
		await this.testSetup.flush();
	}

	captureFrame(): string {
		return this.testSetup.captureCharFrame();
	}

	captureCursor(): { x: number; y: number } {
		const [x, y] = this.testSetup.captureSpans().cursor;
		return { x, y };
	}

	override destroy(): void {
		if (this.destroyed) return;
		this.destroyed = true;
		activeTestRenderers--;
		const treeSitterClient = activeTestRenderers === 0 ? getTreeSitterClient() : undefined;
		super.destroy();
		if (treeSitterClient) testRendererTeardown = treeSitterClient.destroy();
	}
}

export async function createBoneTestRenderer(
	options: BoneRendererOptions & { width?: number; height?: number } = {},
): Promise<BoneTestRenderer> {
	await testRendererTeardown;
	const setup = await createTestRenderer({
		width: options.width ?? 80,
		height: options.height ?? 24,
		screenMode: options.screenMode ?? "alternate-screen",
		footerHeight: options.footerHeight,
		useMouse: options.useMouse ?? true,
		kittyKeyboard: options.useKittyKeyboard ?? true,
		exitOnCtrlC: options.exitOnCtrlC ?? false,
		clearOnShutdown: options.clearOnShutdown ?? true,
		targetFps: options.targetFps ?? 60,
		backgroundColor: options.backgroundColor,
	});
	return new BoneTestRendererImpl(setup);
}
