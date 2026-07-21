import type { AgentMessage } from "@frelion/bone-agent-core";
import { describe, expect, it } from "vitest";
import type { SessionEntry } from "../src/core/session-manager.ts";
import { groupSessionEntriesForRendering } from "../src/modes/interactive/interactive-mode.ts";

function entry(id: string, parentId: string | null, message: AgentMessage): SessionEntry {
	return { type: "message", id, parentId, timestamp: new Date().toISOString(), message };
}

describe("session history pagination", () => {
	it("keeps an assistant tool exchange in the same user turn", () => {
		const entries: SessionEntry[] = [
			entry("u1", null, { role: "user", content: [{ type: "text", text: "first" }], timestamp: 1 }),
			entry("a1", "u1", {
				role: "assistant",
				content: [{ type: "toolCall", id: "tool-1", name: "read", arguments: {} }],
				provider: "test",
				model: "test",
				api: "openai-responses",
				usage: {
					input: 0,
					output: 0,
					cacheRead: 0,
					cacheWrite: 0,
					totalTokens: 0,
					cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
				},
				stopReason: "toolUse",
				timestamp: 2,
			}),
			entry("t1", "a1", {
				role: "toolResult",
				toolCallId: "tool-1",
				toolName: "read",
				content: [{ type: "text", text: "result" }],
				isError: false,
				timestamp: 3,
			}),
			entry("a2", "t1", {
				role: "assistant",
				content: [{ type: "text", text: "done" }],
				provider: "test",
				model: "test",
				api: "openai-responses",
				usage: {
					input: 0,
					output: 0,
					cacheRead: 0,
					cacheWrite: 0,
					totalTokens: 0,
					cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
				},
				stopReason: "stop",
				timestamp: 4,
			}),
			entry("u2", "a2", { role: "user", content: [{ type: "text", text: "second" }], timestamp: 5 }),
		];

		const groups = groupSessionEntriesForRendering(entries);
		expect(groups).toHaveLength(2);
		expect(groups[0]?.map((item) => item.id)).toEqual(["u1", "a1", "t1", "a2"]);
		expect(groups[1]?.map((item) => item.id)).toEqual(["u2"]);
	});
});
