/** OpenTUI config selector for `bone config`. */
import type { OverlayHandle } from "@frelion/bone-tui";
import type { KeyEvent } from "@opentui/core";
import type { SettingsManager } from "../core/settings-manager.ts";
import {
	OpenTUIConfigSelectorV2,
	type ScopedResolvedPaths,
} from "../modes/interactive/components/opentui-config-selector.ts";
import { matchesOpenTUIAction } from "../modes/interactive/opentui-keymap.ts";
import { initTheme, stopThemeWatcher } from "../modes/interactive/theme/theme.ts";
import { clearStartupTui, createStartupTui, startStartupTui } from "./startup-ui.ts";

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
		let handle: OverlayHandle | undefined;
		const close = async () => {
			if (resolved) return;
			resolved = true;
			await handle?.close();
			await clearStartupTui(ui);
			stopThemeWatcher();
			resolve();
		};
		const selector = new OpenTUIConfigSelectorV2({
			...options,
			onClose: () => void close(),
			onExit: () => {
				void close().then(() => process.exit(0));
			},
		});
		const onKey = (event: KeyEvent): boolean => {
			for (const action of ["confirm", "cancel", "up", "down", "pageUp", "pageDown"] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				selector.handleAction(action);
				return true;
			}
			for (const [action, command] of [
				["startupScope", "scope"],
				["startupExit", "exit"],
				["configToggle", "toggle"],
			] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				selector.handleCommand(command);
				return true;
			}
			return false;
		};
		void ui.overlays
			.openAsync((renderer) => ({ root: selector.build(renderer), focusTarget: selector.focusTarget ?? null }), {
				restoreFocus: null,
				onKey,
			})
			.then((opened) => {
				handle = opened;
				startStartupTui(ui, options.settingsManager);
			})
			.catch(() => void close());
	});
}
