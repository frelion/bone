import { Type } from "typebox";
import { Value } from "typebox/value";
import { describe, expect, it, vi } from "vitest";
import { ForgeError } from "../src/core/forge/errors.ts";
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
import { createToolDefinitionFromAgentTool, wrapToolDefinition } from "../src/core/tools/tool-definition-wrapper.ts";

describe("Forge built-in tools", () => {
	it("exposes closed operation-specific schemas that reject placeholder bags", () => {
		const definitions = createForgeToolDefinitions({ cwd: "/workspace", service: { execute: vi.fn() } });

		expect(Value.Check(definitions.forge_context.parameters, {})).toBe(true);
		expect(Value.Check(definitions.forge_context.parameters, { remote: "" })).toBe(false);
		expect(Value.Check(definitions.forge_audit.parameters, {})).toBe(false);
		expect(Value.Check(definitions.forge_audit.parameters, { workflow: "current" })).toBe(true);
		expect(Value.Check(definitions.forge_audit.parameters, { workflow: "merge" })).toBe(false);
		expect(
			Value.Check(definitions.forge_query.parameters, {
				resource: "issue",
				id: "",
				ids: [0],
				state: "opened",
				search: "x",
				limit: 50,
			}),
		).toBe(false);
		for (const input of [
			{ operation: "list", resource: "issue", state: "open" },
			{ operation: "list", resource: "pipeline", state: "failed" },
			{ operation: "list", resource: "job", parentId: 9 },
			{ operation: "get", resource: "issue", id: 42 },
			{ operation: "get_many", resource: "issue", ids: [42, 43] },
		]) {
			expect(Value.Check(definitions.forge_query.parameters, input)).toBe(true);
		}
		expect(Value.Check(definitions.forge_query.parameters, { operation: "list", resource: "job" })).toBe(false);
		expect(
			Value.Check(definitions.forge_query.parameters, {
				operation: "list",
				resource: "job",
				parentId: 9,
				state: "failed",
			}),
		).toBe(false);

		expect(
			Value.Check(definitions.forge_issue.parameters, {
				action: "create",
				requestId: "issue-1",
				input: {},
			}),
		).toBe(false);
		expect(
			Value.Check(definitions.forge_issue.parameters, {
				action: "update",
				requestId: "issue-update-1",
				issueNumber: 7,
				input: {},
			}),
		).toBe(false);
		expect(
			Value.Check(definitions.forge_issue.parameters, {
				action: "comment",
				requestId: "comment-1",
				issueNumber: 7,
				input: { body: "Focused comment" },
			}),
		).toBe(true);
		expect(
			Value.Check(definitions.forge_issue.parameters, {
				action: "close",
				requestId: "close-1",
				issueNumber: 7,
				input: {},
			}),
		).toBe(false);
		expect(
			Value.Check(definitions.forge_pipeline.parameters, {
				action: "retry",
				requestId: "retry-1",
				pipelineId: 9,
			}),
		).toBe(true);
		expect(
			Value.Check(definitions.forge_change.parameters, {
				action: "update",
				requestId: "change-draft",
				changeNumber: 7,
				input: { draft: false },
			}),
		).toBe(false);
		expect(
			Value.Check(definitions.forge_wiki.parameters, {
				action: "update",
				requestId: "wiki-1",
				wikiSlug: "operations/runbook",
				input: { content: "Updated" },
			}),
		).toBe(true);
		expect(
			Value.Check(definitions.forge_wiki.parameters, {
				action: "update",
				requestId: "wiki-empty",
				wikiSlug: "operations/runbook",
				input: {},
			}),
		).toBe(false);
		expect(
			Value.Check(definitions.forge_transition.parameters, {
				transition: "release",
				requestId: "release-1",
				input: { tagName: "v1.0.0", name: "Version 1" },
			}),
		).toBe(true);
		expect(
			Value.Check(definitions.forge_transition.parameters, {
				transition: "release",
				requestId: "release-1",
				input: { name: "Version 1" },
			}),
		).toBe(false);
	});

	it("attaches Agent Tool Contract v1 metadata to every Forge definition", () => {
		const definitions = createForgeToolDefinitions({ cwd: "/workspace", service: { execute: vi.fn() } });

		for (const definition of Object.values(definitions)) {
			expect(definition.contract).toMatchObject({
				version: 1,
				retry: { rejectUnchangedRetry: true },
				outputBudget: { defaultBytes: expect.any(Number), maximumBytes: expect.any(Number) },
			});
			const schema = definition.parameters as {
				additionalProperties?: boolean;
				anyOf?: Array<{ additionalProperties?: boolean }>;
			};
			expect(
				schema.anyOf
					? schema.anyOf.every((variant) => variant.additionalProperties === false)
					: schema.additionalProperties === false,
			).toBe(true);
		}
	});

	it("preserves retry policy when adapting a plain AgentTool through ToolDefinition", () => {
		const parameters = Type.Object({ id: Type.Integer({ minimum: 1 }) }, { additionalProperties: false });
		const retryPolicy = { maxAttempts: 2, rejectUnchangedRetry: true };
		const tool = {
			name: "external_get_issue",
			label: "External get issue",
			description: "Get an external issue",
			parameters,
			retryPolicy,
			execute: vi.fn(),
		};

		expect(wrapToolDefinition(createToolDefinitionFromAgentTool(tool)).retryPolicy).toEqual(retryPolicy);
	});

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
			{ operation: "list", resource: "issue", limit: 10 },
			controller.signal,
			undefined,
			{
				hasUI: true,
				isProjectTrusted: () => true,
				uiV2: { dialogs: { confirm } },
			} as never,
		);

		expect(execute).toHaveBeenCalledWith(
			"forge_query",
			{ operation: "list", resource: "issue", limit: 10 },
			controller.signal,
			{
				cwd: "/workspace",
				agentDir: "/agent",
				toolCallId: "call-1",
				interactive: true,
				projectTrusted: true,
				confirm: expect.any(Function),
			},
		);
		const context = execute.mock.calls[0][3];
		await context.confirm("Merge", "Merge change 42?");
		expect(confirm).toHaveBeenCalledWith({ title: "Merge", message: "Merge change 42?" });
		expect(output).toEqual({
			content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
			details: result,
		});
	});

	it("maps public mutation identifiers and returns only a compact receipt", async () => {
		const execute = vi.fn<ForgeService["execute"]>().mockResolvedValue({
			id: 31,
			number: 12,
			state: "open",
			html_url: "https://github.com/acme/widget/issues/12",
			body: `large provider body ${"x".repeat(20_000)}`,
			user: { login: "octo", token: "secret" },
		});
		const definition = createForgeToolDefinitions({ cwd: "/workspace", service: { execute } }).forge_issue;

		const output = await definition.execute(
			"issue-call",
			{
				action: "comment",
				requestId: "comment-12",
				issueNumber: 12,
				input: { body: "Focused comment" },
			},
			undefined,
			undefined,
			undefined,
		);

		expect(execute).toHaveBeenCalledWith(
			"forge_issue",
			{
				action: "comment",
				requestId: "comment-12",
				id: 12,
				input: { body: "Focused comment" },
			},
			undefined,
			expect.any(Object),
		);
		expect(output.details).toEqual({
			ok: true,
			resource: "issue",
			action: "comment",
			id: 31,
			number: 12,
			state: "open",
			webUrl: "https://github.com/acme/widget/issues/12",
		});
		expect(JSON.stringify(output)).not.toMatch(/large provider body|secret|user/);
	});

	it("returns Forge conflicts through the structured non-retryable error contract", async () => {
		const execute = vi
			.fn<ForgeService["execute"]>()
			.mockRejectedValue(new ForgeError("conflict", "requestId reused"));
		const definition = createForgeToolDefinitions({ cwd: "/workspace", service: { execute } }).forge_issue;

		await expect(
			definition.execute(
				"conflict-call",
				{ action: "close", requestId: "same-request", issueNumber: 7 },
				undefined,
				undefined,
				undefined,
			),
		).rejects.toThrow('"code":"conflict"');
		await expect(
			definition.execute(
				"conflict-call-2",
				{ action: "close", requestId: "different-request", issueNumber: 7 },
				undefined,
				undefined,
				undefined,
			),
		).rejects.toThrow('"retryable":false');
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

		const output = await definition.execute(
			"large-result",
			{ operation: "list", resource: "issue" },
			undefined,
			undefined,
			undefined,
		);
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
