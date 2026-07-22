/**
 * RPC Extension UI Demo
 *
 * Purpose-built extension that exercises all RPC-supported extension UI methods.
 * Designed to be loaded alongside the rpc-extension-ui-example.ts script to
 * demonstrate the full extension UI protocol.
 *
 * UI methods exercised:
 * - select() - on tool_call for dangerous bash commands
 * - confirm() - on session_before_switch
 * - input() - via /rpc-input command
 * - editor() - via /rpc-editor command
 * - notify() - after each dialog completes
 * - setTitle() - on session_start
 * - editor.setText() - via /rpc-prefill command
 */

import type { ExtensionAPI } from "@frelion/bone-coding-agent";

export default function (pi: ExtensionAPI) {
	// -- setTitle on session lifecycle --

	pi.on("session_start", async (event, ctx) => {
		ctx.uiV2.chrome.setTitle(event.reason === "new" ? "pi RPC Demo (new session)" : "pi RPC Demo");
	});

	// -- select on dangerous tool calls --

	pi.on("tool_call", async (event, ctx) => {
		if (event.toolName !== "bash") return undefined;

		const command = event.input.command as string;
		const isDangerous = /\brm\s+(-rf?|--recursive)/i.test(command) || /\bsudo\b/i.test(command);

		if (isDangerous) {
			if (!ctx.hasUI) {
				return { block: true, reason: "Dangerous command blocked (no UI)" };
			}

			const choice = await ctx.uiV2.dialogs.select({
				title: `Dangerous command: ${command}`,
				options: [
					{ value: "allow", label: "Allow" },
					{ value: "block", label: "Block" },
				],
			});
			if (choice !== "allow") {
				ctx.uiV2.dialogs.notify("Command blocked by user", "warning");
				return { block: true, reason: "Blocked by user" };
			}
			ctx.uiV2.dialogs.notify("Command allowed", "info");
		}

		return undefined;
	});

	// -- confirm on session clear --

	pi.on("session_before_switch", async (event, ctx) => {
		if (event.reason !== "new") return;
		if (!ctx.hasUI) return;

		const confirmed = await ctx.uiV2.dialogs.confirm({
			title: "Clear session?",
			message: "All messages will be lost.",
		});
		if (!confirmed) {
			ctx.uiV2.dialogs.notify("Clear cancelled", "info");
			return { cancel: true };
		}
	});

	// -- input via command --

	pi.registerCommand("rpc-input", {
		description: "Prompt for text input through the structured UI service",
		handler: async (_args, ctx) => {
			const value = await ctx.uiV2.dialogs.input({ title: "Enter a value", placeholder: "type something..." });
			if (value) {
				ctx.uiV2.dialogs.notify(`You entered: ${value}`, "info");
			} else {
				ctx.uiV2.dialogs.notify("Input cancelled", "info");
			}
		},
	});

	// -- editor via command --

	pi.registerCommand("rpc-editor", {
		description: "Open a multi-line editor through the structured UI service",
		handler: async (_args, ctx) => {
			const text = await ctx.uiV2.editor.open({
				title: "Edit some text",
				initialValue: "Line 1\nLine 2\nLine 3",
				multiline: true,
			});
			if (text) {
				ctx.uiV2.dialogs.notify(`Editor submitted (${text.split("\n").length} lines)`, "info");
			} else {
				ctx.uiV2.dialogs.notify("Editor cancelled", "info");
			}
		},
	});

	// -- setEditorText via command --

	pi.registerCommand("rpc-prefill", {
		description: "Prefill the composer through the structured UI service",
		handler: async (_args, ctx) => {
			ctx.uiV2.editor.setText("This text was set by the rpc-demo extension.");
			ctx.uiV2.dialogs.notify("Editor prefilled", "info");
		},
	});
}
