/** OpenTUI conversation selector for --resume. */
import type { OverlayHandle } from "@frelion/bone-tui";
import type { KeyEvent } from "@opentui/core";
import type { SessionInfo, SessionListProgress } from "../core/session-manager.ts";
import type { SettingsManager } from "../core/settings-manager.ts";
import { OpenTUISessionPickerV2 } from "../modes/interactive/components/opentui-session-picker.ts";
import { matchesOpenTUIAction } from "../modes/interactive/opentui-keymap.ts";
import { clearStartupTui, createStartupTui, startStartupTui } from "./startup-ui.ts";

type SessionsLoader = (onProgress?: SessionListProgress) => Promise<SessionInfo[]>;

export async function selectSession(
	currentSessionsLoader: SessionsLoader,
	allSessionsLoader: SessionsLoader,
	settingsManager: SettingsManager,
): Promise<string | null> {
	const ui = await createStartupTui(settingsManager);
	return new Promise((resolve) => {
		let settled = false;
		let handle: OverlayHandle | undefined;
		const finish = async (path: string | null) => {
			if (settled) return;
			settled = true;
			await handle?.close();
			await clearStartupTui(ui);
			resolve(path);
		};
		const picker = new OpenTUISessionPickerV2({
			currentSessionsLoader,
			allSessionsLoader,
			onSelect: (path) => void finish(path),
			onCancel: () => void finish(null),
			onExit: () => {
				void finish(null).then(() => process.exit(0));
			},
		});
		const onKey = (event: KeyEvent): boolean => {
			for (const action of ["confirm", "cancel", "up", "down", "pageUp", "pageDown"] as const) {
				if (!matchesOpenTUIAction(event, action)) continue;
				picker.handleAction(action);
				return true;
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
				picker.handleCommand(command);
				return true;
			}
			return false;
		};
		void ui.overlays
			.openAsync((renderer) => ({ root: picker.build(renderer), focusTarget: picker.focusTarget ?? null }), {
				restoreFocus: null,
				onKey,
			})
			.then((opened) => {
				handle = opened;
				startStartupTui(ui, settingsManager);
			})
			.catch(() => void finish(null));
	});
}
