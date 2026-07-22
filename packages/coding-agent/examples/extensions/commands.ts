/**
 * Commands Extension
 *
 * Demonstrates the pi.getCommands() API by providing a /commands command
 * that lists all available slash commands in the current session.
 *
 * Usage:
 * 1. Copy this file to ~/.pi/agent/extensions/ or your project's .pi/extensions/
 * 2. Use /commands to see available commands
 * 3. Use /commands extensions to filter by source
 */

import type { ExtensionAPI, SlashCommandInfo } from "@frelion/bone-coding-agent";

export default function commandsExtension(pi: ExtensionAPI) {
	pi.registerCommand("commands", {
		description: "List available slash commands",
		getArgumentCompletions: (prefix) => {
			const sources = ["extension", "prompt", "skill"];
			const filtered = sources.filter((s) => s.startsWith(prefix));
			return filtered.length > 0 ? filtered.map((s) => ({ value: s, label: s })) : null;
		},
		handler: async (args, ctx) => {
			const commands = pi.getCommands();
			const sourceFilter = args.trim() as "extension" | "prompt" | "skill" | "";

			// Filter by source if specified
			const filtered = sourceFilter ? commands.filter((c) => c.source === sourceFilter) : commands;

			if (filtered.length === 0) {
				ctx.uiV2.dialogs.notify(sourceFilter ? `No ${sourceFilter} commands found` : "No commands found", "info");
				return;
			}

			// Build selection items grouped by source
			const formatCommand = (cmd: SlashCommandInfo): string => {
				const desc = cmd.description ? ` - ${cmd.description}` : "";
				return `/${cmd.name}${desc}`;
			};

			const items: Array<{ value: string; label: string; disabled?: boolean }> = [];
			const sources: Array<{ key: "extension" | "prompt" | "skill"; label: string }> = [
				{ key: "extension", label: "Extensions" },
				{ key: "prompt", label: "Prompts" },
				{ key: "skill", label: "Skills" },
			];

			for (const { key, label } of sources) {
				const cmds = filtered.filter((c) => c.source === key);
				if (cmds.length > 0) {
					items.push({ value: `header:${key}`, label, disabled: true });
					items.push(...cmds.map((command) => ({ value: command.name, label: formatCommand(command) })));
				}
			}

			// Show in a selector (user can scroll and see all commands)
			const selected = await ctx.uiV2.dialogs.select({ title: "Available Commands", options: items });

			if (selected) {
				const cmd = commands.find((command) => command.name === selected);
				if (cmd?.sourceInfo.path) {
					const showPath = await ctx.uiV2.dialogs.confirm({
						title: cmd.name,
						message: `View source path?\n${cmd.sourceInfo.path}`,
					});
					if (showPath) {
						ctx.uiV2.dialogs.notify(cmd.sourceInfo.path, "info");
					}
				}
			}
		},
	});
}
