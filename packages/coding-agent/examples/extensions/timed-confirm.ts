/**
 * Example extension demonstrating timed dialogs with live countdown.
 *
 * Commands:
 * - /timed - Shows confirm dialog that auto-cancels after 5 seconds with countdown
 * - /timed-select - Shows select dialog that auto-cancels after 10 seconds with countdown
 * - /timed-signal - Shows confirm using AbortSignal (manual approach)
 */

import type { ExtensionAPI } from "@frelion/bone-coding-agent";

export default function (pi: ExtensionAPI) {
	// Simple approach: use timeout option (recommended)
	pi.registerCommand("timed", {
		description: "Show a timed confirmation dialog (auto-cancels in 5s with countdown)",
		handler: async (_args, ctx) => {
			const confirmed = await ctx.uiV2.dialogs.confirm({
				title: "Timed Confirmation",
				message: "This dialog will auto-cancel in 5 seconds. Confirm?",
				timeoutMs: 5000,
			});

			if (confirmed) {
				ctx.uiV2.dialogs.notify("Confirmed by user!", "info");
			} else {
				ctx.uiV2.dialogs.notify("Cancelled or timed out", "info");
			}
		},
	});

	pi.registerCommand("timed-select", {
		description: "Show a timed select dialog (auto-cancels in 10s with countdown)",
		handler: async (_args, ctx) => {
			const choice = await ctx.uiV2.dialogs.select({
				title: "Pick an option",
				options: [
					{ value: "a", label: "Option A" },
					{ value: "b", label: "Option B" },
					{ value: "c", label: "Option C" },
				],
				timeoutMs: 10000,
			});

			if (choice) {
				ctx.uiV2.dialogs.notify(`Selected: ${choice}`, "info");
			} else {
				ctx.uiV2.dialogs.notify("Selection cancelled or timed out", "info");
			}
		},
	});

	// Manual approach: use AbortSignal for more control
	pi.registerCommand("timed-signal", {
		description: "Show a timed confirm using AbortSignal (manual approach)",
		handler: async (_args, ctx) => {
			const controller = new AbortController();
			const timeoutId = setTimeout(() => controller.abort(), 5000);

			ctx.uiV2.dialogs.notify("Dialog will auto-cancel in 5 seconds...", "info");

			const confirmed = await ctx.uiV2.dialogs.confirm({
				title: "Timed Confirmation",
				message: "This dialog will auto-cancel in 5 seconds. Confirm?",
				signal: controller.signal,
			});

			clearTimeout(timeoutId);

			if (confirmed) {
				ctx.uiV2.dialogs.notify("Confirmed by user!", "info");
			} else if (controller.signal.aborted) {
				ctx.uiV2.dialogs.notify("Dialog timed out (auto-cancelled)", "warning");
			} else {
				ctx.uiV2.dialogs.notify("Cancelled by user", "info");
			}
		},
	});
}
