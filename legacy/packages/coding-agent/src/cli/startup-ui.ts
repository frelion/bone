import { join } from "node:path";
import { createRenderer, type OverlayHandle, OverlayManager } from "@frelion/bone-tui";
import {
	type BoxRenderable,
	type CliRenderer,
	InputRenderable,
	InputRenderableEvents,
	type KeyEvent,
	type Renderable,
} from "@opentui/core";
import { existsSync, readdirSync, statSync } from "fs";
import { getAgentDir, getSettingsPath } from "../config.ts";
import type { SettingsManager } from "../core/settings-manager.ts";
import {
	createOpenTUIDialogShell,
	type OpenTUIDialogShellOptions,
} from "../modes/interactive/components/opentui-dialog-v2.ts";
import {
	type FirstTimeSetupResult,
	OpenTUIFirstTimeSetupV2,
} from "../modes/interactive/components/opentui-first-time-setup.ts";
import { OpenTUISelectorViewV2 } from "../modes/interactive/components/opentui-selector-v2.ts";
import { matchesOpenTUIAction } from "../modes/interactive/opentui-keymap.ts";
import {
	detectTerminalBackgroundFromEnv,
	initTheme,
	loadThemeFromPath,
	resolveThemeSetting,
	setRegisteredThemes,
	type Theme,
} from "../modes/interactive/theme/theme.ts";

function loadThemes(paths: string[]): Theme[] {
	const themes: Theme[] = [];
	const seen = new Set<string>();
	for (const path of paths) {
		try {
			const loadedTheme = loadThemeFromPath(path);
			if (loadedTheme.name) {
				if (seen.has(loadedTheme.name)) continue;
				seen.add(loadedTheme.name);
			}
			themes.push(loadedTheme);
		} catch {
			// Resource loading reports broken theme files after startup.
		}
	}
	return themes;
}

async function loadStartupThemes(settingsManager: SettingsManager): Promise<Theme[]> {
	const paths: string[] = [];
	const addDirectory = (directory: string) => {
		if (!existsSync(directory)) return;
		for (const entry of readdirSync(directory, { withFileTypes: true })) {
			const path = join(directory, entry.name);
			if ((entry.isFile() || entry.isSymbolicLink()) && path.endsWith(".json")) paths.push(path);
		}
	};
	addDirectory(join(getAgentDir(), "themes"));
	for (const path of settingsManager.getGlobalSettings().themes ?? []) {
		const resolved = path.startsWith("~") ? join(process.env.HOME ?? "", path.slice(2)) : path;
		if (existsSync(resolved) && statSync(resolved).isDirectory()) addDirectory(resolved);
		else paths.push(resolved);
	}
	return loadThemes(paths);
}

export interface StartupTui {
	readonly renderer: CliRenderer;
	readonly overlays: OverlayManager;
}

export async function createStartupTui(settingsManager: SettingsManager): Promise<StartupTui> {
	setRegisteredThemes(await loadStartupThemes(settingsManager));
	const terminalTheme = detectTerminalBackgroundFromEnv().theme;
	initTheme(resolveThemeSetting(settingsManager.getThemeSetting(), terminalTheme) ?? terminalTheme);
	const renderer = await createRenderer({ screenMode: "alternate-screen" });
	return { renderer, overlays: new OverlayManager(renderer) };
}

export function startStartupTui(ui: StartupTui, _settingsManager?: SettingsManager): void {
	ui.renderer.start();
}

export async function clearStartupTui(ui: StartupTui): Promise<void> {
	await ui.overlays.dispose();
	ui.renderer.stop();
	ui.renderer.destroy();
}

/** First-time setup is currently disabled for this distribution. */
export function shouldRunFirstTimeSetup(settingsPath: string = getSettingsPath()): boolean {
	void settingsPath;
	return false;
}

interface StartupOverlayView {
	root: BoxRenderable;
	focusTarget?: Renderable;
}

function finishOnEscape(finish: () => void): (event: KeyEvent) => boolean {
	return (event) => {
		if (!matchesOpenTUIAction(event, "cancel")) return false;
		finish();
		return true;
	};
}

async function openStartupOverlay(
	ui: StartupTui,
	create: (renderer: CliRenderer) => StartupOverlayView,
	onKey: (event: KeyEvent) => boolean,
): Promise<OverlayHandle | undefined> {
	try {
		return await ui.overlays.openAsync((renderer) => create(renderer), { restoreFocus: null, onKey });
	} catch {
		return undefined;
	}
}

export async function showStartupSelector<T>(
	settingsManager: SettingsManager,
	title: string,
	options: Array<{ label: string; value: T }>,
): Promise<T | undefined> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let handle: OverlayHandle | undefined;
		const finish = async (result: T | undefined) => {
			if (settled) return;
			settled = true;
			await handle?.close();
			await clearStartupTui(ui);
			resolve(result);
		};
		const selector = new OpenTUISelectorViewV2({
			title,
			items: options,
			onSelect: (value) => void finish(value),
			onCancel: () => void finish(undefined),
		});
		handle = undefined;
		void openStartupOverlay(
			ui,
			(renderer) => ({ root: selector.build(renderer), focusTarget: selector.focusTarget }),
			finishOnEscape(() => void finish(undefined)),
		).then((opened) => {
			handle = opened;
			startStartupTui(ui, settingsManager);
		});
	});
}

export async function showFirstTimeSetup(settingsManager: SettingsManager): Promise<void> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let handle: OverlayHandle | undefined;
		const finish = async (result: FirstTimeSetupResult | undefined) => {
			if (settled) return;
			settled = true;
			if (result) {
				settingsManager.setTheme(result.theme);
				settingsManager.setEnableAnalytics(result.shareAnalytics);
				await settingsManager.flush();
			}
			await handle?.close();
			await clearStartupTui(ui);
			resolve();
		};
		const setup = new OpenTUIFirstTimeSetupV2({
			detectedTheme: detectTerminalBackgroundFromEnv().theme,
			onSubmit: (result) => void finish(result),
			onCancel: () => void finish(undefined),
			onFocusTargetChange: (target) => target.focus(),
		});
		void openStartupOverlay(
			ui,
			(renderer) => ({ root: setup.build(renderer), focusTarget: setup.focusTarget }),
			finishOnEscape(() => void finish(undefined)),
		).then((opened) => {
			handle = opened;
			startStartupTui(ui, settingsManager);
		});
	});
}

interface StartupInputView extends StartupOverlayView {
	input: InputRenderable;
}

function createStartupInput(
	renderer: CliRenderer,
	title: string,
	placeholder: string | undefined,
	onSubmit: (value: string) => void,
): StartupInputView {
	const dialogOptions: OpenTUIDialogShellOptions = { title, footer: "submit · cancel" };
	const dialog = createOpenTUIDialogShell(renderer, dialogOptions);
	const input = new InputRenderable(renderer, { width: "100%", placeholder });
	input.on(InputRenderableEvents.ENTER, (value: string) => onSubmit(value));
	dialog.body.add(input);
	return { root: dialog.root, focusTarget: input, input };
}

export async function showStartupInput(
	settingsManager: SettingsManager,
	title: string,
	placeholder?: string,
): Promise<string | undefined> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let handle: OverlayHandle | undefined;
		const finish = async (result: string | undefined) => {
			if (settled) return;
			settled = true;
			await handle?.close();
			await clearStartupTui(ui);
			resolve(result);
		};
		void openStartupOverlay(
			ui,
			(renderer) => createStartupInput(renderer, title, placeholder, (value) => void finish(value)),
			finishOnEscape(() => void finish(undefined)),
		).then((opened) => {
			handle = opened;
			startStartupTui(ui, settingsManager);
		});
	});
}
