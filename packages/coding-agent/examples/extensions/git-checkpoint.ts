/**
 * Git Checkpoint Extension
 *
 * Creates git stash checkpoints at each turn so /fork can restore code state.
 * When forking, offers to restore code to that point in history.
 */

import type { ExtensionAPI } from "@frelion/bone-coding-agent";

export default function (pi: ExtensionAPI) {
	const checkpoints = new Map<string, string>();
	let currentEntryId: string | undefined;

	// Track the current entry ID when user messages are saved
	pi.on("tool_result", async (_event, ctx) => {
		const leaf = ctx.sessionManager.getLeafEntry();
		if (leaf) currentEntryId = leaf.id;
	});

	pi.on("turn_start", async () => {
		// Create a git stash entry before LLM makes changes
		const { stdout } = await pi.exec("git", ["stash", "create"]);
		const ref = stdout.trim();
		if (ref && currentEntryId) {
			checkpoints.set(currentEntryId, ref);
		}
	});

	pi.on("session_before_fork", async (event, ctx) => {
		const ref = checkpoints.get(event.entryId);
		if (!ref) return;

		if (!ctx.hasUI) {
			// In non-interactive mode, don't restore automatically
			return;
		}

		const choice = await ctx.uiV2.dialogs.select({
			title: "Restore code state?",
			options: [
				{ value: "restore", label: "Yes, restore code to that point" },
				{ value: "keep", label: "No, keep current code" },
			],
		});

		if (choice === "restore") {
			await pi.exec("git", ["stash", "apply", ref]);
			ctx.uiV2.dialogs.notify("Code restored to checkpoint", "info");
		}
	});

	pi.on("agent_end", async () => {
		// Clear checkpoints after agent completes
		checkpoints.clear();
	});
}
