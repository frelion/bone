/** OpenTUI config selector for `bone config`. */
import type { SettingsManager } from "../core/settings-manager.ts";
import {
	OpenTUIConfigSelectorV2,
	type ScopedResolvedPaths,
} from "../modes/interactive/components/opentui-config-selector.ts";
import { matchesOpenTUIAction } from "../modes/interactive/opentui-keymap.ts";
import { initTheme, stopThemeWatcher } from "../modes/interactive/theme/theme.ts";
import { createStartupTui, startStartupTui } from "./startup-ui.ts";

export interface ConfigSelectorOptions {
	resolvedPaths: ScopedResolvedPaths;
	settingsManager: SettingsManager;
	cwd: string;
	agentDir: string;
	writeScope: "global" | "project";
	projectModeAvailable: boolean;
}

export async function selectConfig(options: ConfigSelectorOptions): Promise<void> {
	initTheme(options.settingsManager.getTheme(), true);
	const ui = await createStartupTui(options.settingsManager);
	return new Promise((resolve) => {
		let resolved = false;
		let unsubscribe = () => {};
		const close = () => {
			if (resolved) return;
			resolved = true;
			unsubscribe();
			ui.stop();
			ui.destroy();
			stopThemeWatcher();
			resolve();
		};
		const selector = new OpenTUIConfigSelectorV2({
			...options,
			onClose: close,
			onExit: () => {
				ui.stop();
				ui.destroy();
				stopThemeWatcher();
				process.exit(0);
			},
		});
		ui.mount(selector);
		unsubscribe = ui.onKey((event) => {
			for (const action of ["confirm", "cancel", "up", "down", "pageUp", "pageDown"] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				event.preventDefault();
				event.stopPropagation();
				selector.handleAction(action);
				return;
			}
			for (const [action, command] of [
				["startupScope", "scope"],
				["startupExit", "exit"],
				["configToggle", "toggle"],
			] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				event.preventDefault();
				event.stopPropagation();
				selector.handleCommand(command);
				return;
			}
		});
		startStartupTui(ui, options.settingsManager);
	});
}
