/**
 * Confirm Destructive Actions Extension
 *
 * Prompts for confirmation before destructive session actions (clear, switch, branch).
 * Demonstrates how to cancel session events using the before_* events.
 */

import type { ExtensionAPI, SessionBeforeSwitchEvent, SessionMessageEntry } from "@frelion/bone-coding-agent";

export default function (pi: ExtensionAPI) {
	pi.on("session_before_switch", async (event: SessionBeforeSwitchEvent, ctx) => {
		if (!ctx.hasUI) return;

		if (event.reason === "new") {
			const confirmed = await ctx.uiV2.dialogs.confirm({
				title: "Clear session?",
				message: "This will delete all messages in the current session.",
			});

			if (!confirmed) {
				ctx.uiV2.dialogs.notify("Clear cancelled", "info");
				return { cancel: true };
			}
			return;
		}

		// reason === "resume" - check if there are unsaved changes (messages since last assistant response)
		const entries = ctx.sessionManager.getEntries();
		const hasUnsavedWork = entries.some(
			(e): e is SessionMessageEntry => e.type === "message" && e.message.role === "user",
		);

		if (hasUnsavedWork) {
			const confirmed = await ctx.uiV2.dialogs.confirm({
				title: "Switch session?",
				message: "You have messages in the current session. Switch anyway?",
			});

			if (!confirmed) {
				ctx.uiV2.dialogs.notify("Switch cancelled", "info");
				return { cancel: true };
			}
		}
	});

	pi.on("session_before_fork", async (event, ctx) => {
		if (!ctx.hasUI) return;

		const choice = await ctx.uiV2.dialogs.select({
			title: `Fork from entry ${event.entryId.slice(0, 8)}?`,
			options: [
				{ value: "fork", label: "Yes, create fork" },
				{ value: "stay", label: "No, stay in current session" },
			],
		});

		if (choice !== "fork") {
			ctx.uiV2.dialogs.notify("Fork cancelled", "info");
			return { cancel: true };
		}
	});
}
