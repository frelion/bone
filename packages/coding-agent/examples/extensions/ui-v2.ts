import type { ExtensionAPI } from "@frelion/bone-coding-agent";
import type { BoneView } from "@frelion/bone-tui";

function labelView(content: string): BoneView {
	return {
		mount(context) {
			return context.createText({ content, bold: true });
		},
	};
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

			ui.widgets.set("ui-v2-mode", () => labelView(`Mode: ${mode}`), { placement: "aboveEditor" });
			ui.dialogs.notify(`Selected ${mode} mode`, "info");
		},
	});
}
