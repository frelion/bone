import { Box, Container, Markdown, type MarkdownTheme, Spacer, Text } from "@frelion/bone-tui";
import type { PlanProposal } from "../../../core/plan-mode.ts";
import { getMarkdownTheme, theme } from "../theme/theme.ts";

export class PlanProposalComponent extends Container {
	constructor(proposal: PlanProposal, markdownTheme: MarkdownTheme = getMarkdownTheme()) {
		super();
		this.addChild(new Spacer(1));
		const box = new Box(1, 1, (text) => theme.bg("customMessageBg", text));
		box.addChild(new Text(theme.bold(theme.fg("accent", `Plan v${proposal.version}`)), 0, 0));
		box.addChild(new Spacer(1));
		box.addChild(new Markdown(proposal.content, 0, 0, markdownTheme));
		this.addChild(box);
	}
}
