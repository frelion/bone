/** OpenTUI conversation selector for --resume. */
import type { SessionInfo, SessionListProgress } from "../core/session-manager.ts";
import type { SettingsManager } from "../core/settings-manager.ts";
import { OpenTUISessionPickerV2 } from "../modes/interactive/components/opentui-session-picker.ts";
import { matchesOpenTUIAction } from "../modes/interactive/opentui-keymap.ts";
import { createStartupTui, startStartupTui } from "./startup-ui.ts";

type SessionsLoader = (onProgress?: SessionListProgress) => Promise<SessionInfo[]>;

export async function selectSession(
	currentSessionsLoader: SessionsLoader,
	allSessionsLoader: SessionsLoader,
	settingsManager: SettingsManager,
): Promise<string | null> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let unsubscribe = () => {};
		const finish = (path: string | null) => {
			if (settled) return;
			settled = true;
			unsubscribe();
			ui.stop();
			ui.destroy();
			resolve(path);
		};
		const picker = new OpenTUISessionPickerV2({
			currentSessionsLoader,
			allSessionsLoader,
			onSelect: (path) => finish(path),
			onCancel: () => finish(null),
			onExit: () => {
				ui.stop();
				ui.destroy();
				process.exit(0);
			},
		});
		ui.mount(picker);
		unsubscribe = ui.onKey((event) => {
			for (const action of ["confirm", "cancel", "up", "down", "pageUp", "pageDown"] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				event.preventDefault();
				event.stopPropagation();
				picker.handleAction(action);
				return;
			}
			for (const [action, command] of [
				["startupScope", "scope"],
				["startupExit", "exit"],
				["sessionSort", "sort"],
				["sessionNamedFilter", "named"],
				["sessionPath", "path"],
				["sessionDelete", "delete"],
			] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				event.preventDefault();
				event.stopPropagation();
				picker.handleCommand(command);
				return;
			}
		});
		startStartupTui(ui, settingsManager);
	});
}
