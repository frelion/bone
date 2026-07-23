import { type CliRenderer, type CliRendererConfig, createCliRenderer } from "@opentui/core";

export type RendererOptions = Omit<CliRendererConfig, "autoFocus">;

/** Create the application renderer with Bone's terminal defaults and explicit focus ownership. */
export async function createRenderer(options: RendererOptions = {}): Promise<CliRenderer> {
	return createCliRenderer({
		...options,
		screenMode: options.screenMode ?? "alternate-screen",
		useMouse: options.useMouse ?? true,
		useKittyKeyboard: options.useKittyKeyboard === undefined ? {} : options.useKittyKeyboard,
		exitOnCtrlC: options.exitOnCtrlC ?? false,
		clearOnShutdown: options.clearOnShutdown ?? true,
		targetFps: options.targetFps ?? 60,
		autoFocus: false,
	});
}
