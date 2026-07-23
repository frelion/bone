import type { ExtensionAPI } from "@frelion/bone-coding-agent";
import { type CliRenderer, TextAttributes, TextRenderable } from "@opentui/core";

function labelView(content: string): (renderer: CliRenderer) => TextRenderable {
	return (renderer) => new TextRenderable(renderer, { content, attributes: TextAttributes.BOLD });
}

export default function (bone: ExtensionAPI) {
	bone.registerCommand("ui-v2-demo", {
		description: "Exercise the structured extension UI contract",
		handler: async (_args, context) => {
			const ui = context.uiV2;
			const mode = await ui.dialogs.select({
				title: "Execution mode",
				options: [
					{ value: "safe", label: "Safe", description: "Require confirmation before mutations" },
					{ value: "fast", label: "Fast", description: "Run approved operations immediately" },
				],
			});
			if (!mode) return;

			ui.widgets.set("ui-v2-mode", labelView(`Mode: ${mode}`), { placement: "aboveEditor" });
			ui.dialogs.notify(`Selected ${mode} mode`, "info");
		},
	});
}
