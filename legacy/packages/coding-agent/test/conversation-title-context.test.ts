import type { AgentMessage } from "@frelion/bone-agent-core";
import { fauxAssistantMessage } from "@frelion/bone-ai";
import { describe, expect, it } from "vitest";
import { buildConversationTitleContext } from "../src/core/conversation-title.ts";
import type { SessionEntry } from "../src/core/session-manager.ts";

function messageEntry(message: AgentMessage, index: number): SessionEntry {
	return {
		type: "message",
		id: `entry-${index}`,
		parentId: index === 0 ? null : `entry-${index - 1}`,
		timestamp: new Date(index * 1_000).toISOString(),
		message,
	};
}

function userMessage(content: string): AgentMessage {
	return { role: "user", content, timestamp: Date.now() };
}

describe("buildConversationTitleContext", () => {
	it("keeps the complete user and final-assistant timeline in chronological order", () => {
		const entries = [
			messageEntry(userMessage("First task: improve the Side."), 0),
			messageEntry(fauxAssistantMessage("The Side now supports session focus."), 1),
			messageEntry(userMessage("Now focus on semantic conversation search instead."), 2),
			messageEntry(fauxAssistantMessage("Calling search tools.", { stopReason: "toolUse" }), 3),
			messageEntry(fauxAssistantMessage("Designed semantic search for conversation history."), 4),
		];

		expect(buildConversationTitleContext(entries)).toBe(
			[
				"User:\nFirst task: improve the Side.",
				"Assistant:\nThe Side now supports session focus.",
				"User:\nNow focus on semantic conversation search instead.",
				"Assistant:\nDesigned semantic search for conversation history.",
			].join("\n\n"),
		);
	});

	it("does not apply a message or total-context truncation limit", () => {
		const longUserMessage = "x".repeat(7_000);
		const context = buildConversationTitleContext([messageEntry(userMessage(longUserMessage), 0)]);

		expect(context).toContain(longUserMessage);
	});
});
