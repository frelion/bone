import chalk from "chalk";
import type { ProjectTrustContext } from "../core/extensions/types.ts";
import type { AppMode } from "../core/project-trust.ts";
import type { SettingsManager } from "../core/settings-manager.ts";
import { showStartupInput, showStartupSelector } from "./startup-ui.ts";

export function createProjectTrustContext(options: {
	cwd: string;
	mode: AppMode;
	settingsManager: SettingsManager;
	hasUI: boolean;
}): ProjectTrustContext {
	return {
		cwd: options.cwd,
		mode: options.mode === "interactive" ? "tui" : options.mode,
		hasUI: options.hasUI,
		uiV2: {
			dialogs: {
				select: async (request) => {
					if (!options.hasUI) {
						return undefined;
					}
					if (options.mode !== "interactive") {
						return undefined;
					}
					return showStartupSelector(
						options.settingsManager,
						request.title,
						request.options
							.filter((option) => !option.disabled)
							.map((option) => ({ label: option.label, value: option.value })),
					);
				},
				confirm: async (request) => {
					if (!options.hasUI) {
						return false;
					}
					if (options.mode !== "interactive") {
						return false;
					}
					return (
						(await showStartupSelector(options.settingsManager, `${request.title}\n${request.message}`, [
							{ label: "Yes", value: true },
							{ label: "No", value: false },
						])) ?? false
					);
				},
				input: async (request) => {
					if (!options.hasUI) {
						return undefined;
					}
					if (options.mode !== "interactive") {
						return undefined;
					}
					return showStartupInput(options.settingsManager, request.title, request.placeholder);
				},
				notify: (message, type = "info") => {
					if (options.mode !== "interactive") {
						const color = type === "error" ? chalk.red : type === "warning" ? chalk.yellow : chalk.cyan;
						console.error(color(message));
					}
				},
			},
		},
	};
}
