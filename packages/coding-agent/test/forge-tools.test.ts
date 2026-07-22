import { describe, expect, it, vi } from "vitest";
import {
	createForgeToolDefinitions,
	FORGE_READ_TOOL_NAMES,
	FORGE_TOOL_NAMES,
	FORGE_WRITE_TOOL_NAMES,
	type ForgeService,
} from "../src/core/forge/tools.ts";
import { PLAN_MODE_TOOL_NAMES } from "../src/core/plan-mode.ts";
import { createAllToolDefinitions } from "../src/core/tools/index.ts";

describe("Forge built-in tools", () => {
	it("registers distinct read and mutation tool sets", () => {
		const definitions = createForgeToolDefinitions({ cwd: "/workspace", service: { execute: vi.fn() } });

		expect(Object.keys(definitions)).toEqual(FORGE_TOOL_NAMES);
		for (const name of FORGE_READ_TOOL_NAMES) {
			expect(definitions[name].executionMode).toBe("parallel");
		}
		for (const name of FORGE_WRITE_TOOL_NAMES) {
			expect(definitions[name].executionMode).toBe("sequential");
		}
	});

	it("allows only read-only Forge tools in Plan Mode", () => {
		for (const name of FORGE_READ_TOOL_NAMES) expect(PLAN_MODE_TOOL_NAMES).toContain(name);
		for (const name of FORGE_WRITE_TOOL_NAMES) expect(PLAN_MODE_TOOL_NAMES).not.toContain(name);
	});

	it("routes execution through the injected service with cancellation context", async () => {
		const result = { provider: "gitlab", data: { id: 42 } };
		const execute = vi.fn<ForgeService["execute"]>().mockResolvedValue(result);
		const definitions = createForgeToolDefinitions({ cwd: "/workspace", agentDir: "/agent", service: { execute } });
		const controller = new AbortController();
		const confirm = vi.fn().mockResolvedValue(true);

		const output = await definitions.forge_query.execute(
			"call-1",
			{ resource: "issue", limit: 10 },
			controller.signal,
			undefined,
			{
				hasUI: true,
				isProjectTrusted: () => true,
				ui: { confirm },
			} as never,
		);

		expect(execute).toHaveBeenCalledWith("forge_query", { resource: "issue", limit: 10 }, controller.signal, {
			cwd: "/workspace",
			agentDir: "/agent",
			toolCallId: "call-1",
			interactive: true,
			projectTrusted: true,
			confirm: expect.any(Function),
		});
		const context = execute.mock.calls[0][3];
		await context.confirm("Merge", "Merge change 42?");
		expect(confirm).toHaveBeenCalledWith("Merge", "Merge change 42?");
		expect(output).toEqual({
			content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
			details: result,
		});
	});

	it("participates in the complete built-in definition registry", () => {
		const definitions = createAllToolDefinitions("/workspace", { forge: { service: { execute: vi.fn() } } });
		for (const name of FORGE_TOOL_NAMES) expect(definitions[name].name).toBe(name);
	});
});
