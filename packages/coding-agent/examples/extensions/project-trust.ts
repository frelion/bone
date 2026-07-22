/**
 * Project Trust Extension
 *
 * Demonstrates the project_trust event. Install globally or pass via -e:
 *
 *   mkdir -p ~/.pi/agent/extensions
 *   cp packages/coding-agent/examples/extensions/project-trust.ts ~/.pi/agent/extensions/
 *
 * Or:
 *
 *   pi -e packages/coding-agent/examples/extensions/project-trust.ts
 *
 * Try it in a project containing .pi, AGENTS.md/CLAUDE.md, or .agents/skills.
 */

import type { ExtensionAPI, ProjectTrustEventResult } from "@frelion/bone-coding-agent";

export default function (pi: ExtensionAPI) {
	let loadCount = 0;
	loadCount++;

	// Multiple handlers in one extension are allowed. The first handler that returns
	// { trusted: "yes" } or { trusted: "no" } wins and suppresses the built-in
	// trust prompt. Return { trusted: "undecided" } to let another handler or the
	// built-in flow decide.
	pi.on("project_trust", async (event, ctx): Promise<ProjectTrustEventResult> => {
		ctx.uiV2.dialogs.notify(`project_trust fired for ${event.cwd} (mode: ${ctx.mode}, load: ${loadCount})`, "info");

		if (!ctx.hasUI) {
			return { trusted: "undecided" };
		}

		const choice = await ctx.uiV2.dialogs.select({
			title: `Project trust for:\n${event.cwd}`,
			options: [
				{ value: "trust-remember", label: "Trust and remember" },
				{ value: "trust-note", label: "Trust with note and remember" },
				{ value: "trust-session", label: "Trust this session" },
				{ value: "deny-session", label: "Do not trust this session" },
				{ value: "builtin", label: "Let built-in prompt decide" },
			],
		});

		if (choice === "trust-note") {
			const note = await ctx.uiV2.dialogs.input({
				title: "Project trust note",
				placeholder: "Optional note for this demo",
			});
			ctx.uiV2.dialogs.notify(note ? `Recorded demo note: ${note}` : "No demo note entered", "info");
			return { trusted: "yes", remember: true };
		}
		if (choice === "trust-remember") {
			return { trusted: "yes", remember: true };
		}
		if (choice === "trust-session") {
			return { trusted: "yes" };
		}
		if (choice === "deny-session") {
			return { trusted: "no" };
		}
		if (choice === "builtin") {
			return { trusted: "undecided" };
		}
		return { trusted: "undecided" };
	});

	pi.on("session_start", (_event, ctx) => {
		ctx.uiV2.dialogs.notify(`project-trust example loaded after trust resolution in ${ctx.cwd}`, "info");
	});
}
