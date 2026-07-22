import { describe, expect, it, vi } from "vitest";
import { projectForgePage } from "../src/core/forge/result.ts";
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
				uiV2: { dialogs: { confirm } },
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
		expect(confirm).toHaveBeenCalledWith({ title: "Merge", message: "Merge change 42?" });
		expect(output).toEqual({
			content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
			details: result,
		});
	});

	it("participates in the complete built-in definition registry", () => {
		const definitions = createAllToolDefinitions("/workspace", { forge: { service: { execute: vi.fn() } } });
		for (const name of FORGE_TOOL_NAMES) expect(definitions[name].name).toBe(name);
	});

	it("bounds serialized Forge tool content and details to 64 KiB with valid JSON", async () => {
		const sentinel = "尾部-secret-sentinel";
		const execute = vi
			.fn<ForgeService["execute"]>()
			.mockResolvedValue({ payload: `${"上下文".repeat(80_000)}${sentinel}` });
		const definition = createForgeToolDefinitions({ cwd: "/workspace", service: { execute } }).forge_query;

		const output = await definition.execute("large-result", { resource: "issue" }, undefined, undefined, undefined);
		const content = output.content[0];
		if (content?.type !== "text") throw new Error("Expected text Forge result");
		const parsed = JSON.parse(content.text) as Record<string, unknown>;

		expect(Buffer.byteLength(content.text, "utf8")).toBeLessThanOrEqual(64 * 1024);
		expect(parsed).toMatchObject({ truncated: true, reason: "output_budget", maximumBytes: 64 * 1024 });
		expect(JSON.stringify(output.details)).not.toContain(sentinel);
	});

	it("omits complete list items with structured metadata when summaries exceed the page budget", () => {
		const result = projectForgePage("issue", {
			items: Array.from({ length: 50 }, (_, index) => ({
				id: index + 1,
				title: `Issue ${index} ${"界".repeat(2_000)}`,
				web_url: `https://gitlab.example/issues/${index}/${"x".repeat(3_000)}`,
			})),
			hasMore: false,
		});

		expect(Buffer.byteLength(JSON.stringify(result), "utf8")).toBeLessThan(64 * 1024);
		expect(result).toMatchObject({ truncated: true, truncationReason: "output_budget", hasMore: true });
		expect(result.omittedItems).toEqual(expect.any(Number));
	});
});
