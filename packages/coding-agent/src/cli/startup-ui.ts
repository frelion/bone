import { join } from "node:path";
import {
	type BoneInputNode,
	type BoneNode,
	type BoneRenderContext,
	type BoneRenderer,
	type BoneView,
	createBoneRenderer,
} from "@frelion/bone-tui";
import { existsSync, readdirSync, statSync } from "fs";
import { getAgentDir, getSettingsPath } from "../config.ts";
import type { SettingsManager } from "../core/settings-manager.ts";
import { createOpenTUIDialogShell } from "../modes/interactive/components/opentui-dialog-v2.ts";
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

export async function createStartupTui(settingsManager: SettingsManager): Promise<BoneRenderer> {
	setRegisteredThemes(await loadStartupThemes(settingsManager));
	const terminalTheme = detectTerminalBackgroundFromEnv().theme;
	initTheme(resolveThemeSetting(settingsManager.getThemeSetting(), terminalTheme) ?? terminalTheme);
	return createBoneRenderer({ screenMode: "alternate-screen", useMouse: true, clearOnShutdown: true });
}

export function startStartupTui(ui: BoneRenderer, settingsManager: SettingsManager): void {
	void settingsManager;
	ui.start();
}

async function clearStartupTui(ui: BoneRenderer): Promise<void> {
	ui.content.clear();
	ui.requestRender();
	await ui.idle();
	ui.stop();
	ui.destroy();
}

/** First-time setup is currently disabled for this distribution. */
export function shouldRunFirstTimeSetup(settingsPath: string = getSettingsPath()): boolean {
	void settingsPath;
	return false;
}

function bindSelectorKeys(
	ui: BoneRenderer,
	selector: { handleAction(action: "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown"): boolean },
): () => void {
	return ui.onKey((event) => {
		for (const action of ["confirm", "cancel", "up", "down", "pageUp", "pageDown"] as const) {
			if (!matchesOpenTUIAction(event, action)) continue;
			event.preventDefault();
			event.stopPropagation();
			selector.handleAction(action);
			return;
		}
	});
}

export async function showStartupSelector<T>(
	settingsManager: SettingsManager,
	title: string,
	options: Array<{ label: string; value: T }>,
): Promise<T | undefined> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let unsubscribe = () => {};
		const finish = async (result: T | undefined) => {
			if (settled) return;
			settled = true;
			unsubscribe();
			await clearStartupTui(ui);
			resolve(result);
		};
		const selector = new OpenTUISelectorViewV2({
			title,
			items: options,
			onSelect: (value) => void finish(value),
			onCancel: () => void finish(undefined),
		});
		ui.mount(selector);
		unsubscribe = bindSelectorKeys(ui, selector);
		startStartupTui(ui, settingsManager);
	});
}

export async function showFirstTimeSetup(settingsManager: SettingsManager): Promise<void> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let unsubscribe = () => {};
		const finish = async (result: FirstTimeSetupResult | undefined) => {
			if (settled) return;
			settled = true;
			unsubscribe();
			if (result) {
				settingsManager.setTheme(result.theme);
				settingsManager.setEnableAnalytics(result.shareAnalytics);
				await settingsManager.flush();
			}
			await clearStartupTui(ui);
			resolve();
		};
		const detectedTheme = detectTerminalBackgroundFromEnv().theme;
		const setup = new OpenTUIFirstTimeSetupV2({
			detectedTheme,
			onSubmit: (result) => void finish(result),
			onCancel: () => void finish(undefined),
		});
		ui.mount(setup);
		unsubscribe = bindSelectorKeys(ui, setup);
		startStartupTui(ui, settingsManager);
	});
}

class StartupInputView implements BoneView {
	private readonly title: string;
	private readonly placeholder: string | undefined;
	private readonly onSubmit: (value: string) => void;
	private readonly onCancel: () => void;
	private input: BoneInputNode | undefined;

	constructor(
		title: string,
		placeholder: string | undefined,
		onSubmit: (value: string) => void,
		onCancel: () => void,
	) {
		this.title = title;
		this.placeholder = placeholder;
		this.onSubmit = onSubmit;
		this.onCancel = onCancel;
	}

	mount(context: BoneRenderContext): BoneNode {
		const dialog = createOpenTUIDialogShell(context, { title: this.title, footer: "Enter submit · Esc cancel" });
		this.input = context.createInput({
			width: "100%",
			placeholder: this.placeholder,
			onConfirm: this.onSubmit,
			onCancel: this.onCancel,
		});
		dialog.body.append(this.input);
		this.input.focus();
		return dialog.root;
	}

	handleAction(action: "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown"): boolean {
		if (action === "confirm") this.input?.submit();
		else if (action === "cancel") this.onCancel();
		return true;
	}
}

export async function showStartupInput(
	settingsManager: SettingsManager,
	title: string,
	placeholder?: string,
): Promise<string | undefined> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let unsubscribe = () => {};
		const finish = async (result: string | undefined) => {
			if (settled) return;
			settled = true;
			unsubscribe();
			await clearStartupTui(ui);
			resolve(result);
		};
		const input = new StartupInputView(
			title,
			placeholder,
			(value) => void finish(value),
			() => void finish(undefined),
		);
		ui.mount(input);
		unsubscribe = bindSelectorKeys(ui, input);
		startStartupTui(ui, settingsManager);
	});
}
