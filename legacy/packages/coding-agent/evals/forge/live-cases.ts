import type { LiveForgeEvalCase } from "./types.ts";

const issue = (number: number, body = "Focused issue body") => ({
	id: number,
	iid: number,
	title: number === 42 ? "Login fails" : `Issue ${number}`,
	state: "open",
	body,
	html_url: `https://github.com/example/repo/issues/${number}`,
});

export const liveForgeEvalCases: readonly LiveForgeEvalCase[] = [
	{
		id: "live.query.issue-list",
		category: "query",
		prompt: "列出当前仓库打开的 issue，并告诉我标题。只使用 Forge 工具。",
		service: [{ toolName: "forge_query", input: { operation: "list", resource: "issue", state: "open" }, outcome: { kind: "return", value: { resource: "issue", mode: "list", items: [issue(42)] } } }],
		expectedServiceTools: ["forge_query"],
	},
	{
		id: "live.recovery.legacy-placeholder",
		category: "recovery",
		prompt: "读取 issue 42。为了测试错误恢复，请第一次调用 forge_query 时故意使用旧式占位参数 id=空字符串、ids=[0]；收到校验错误后，必须修改参数并完成任务。",
		service: [{ toolName: "forge_query", input: { operation: "get", resource: "issue", id: 42 }, outcome: { kind: "return", value: { resource: "issue", mode: "detail", item: issue(42) } } }],
		expectedServiceTools: ["forge_query"],
	},
	{
		id: "live.mutation.compact-receipt",
		category: "mutation",
		prompt: "关闭当前仓库的 issue 42。只使用 Forge 工具，并为写操作提供稳定的 requestId。",
		service: [{ toolName: "forge_issue", input: { action: "close", requestId: "close-issue-42", id: 42 }, outcome: { kind: "return", value: { id: 42, iid: 42, state: "closed", providerRawPayload: "must not survive projection" } } }],
		expectedServiceTools: ["forge_issue"],
		mustNotContain: ["providerRawPayload"],
	},
	{
		id: "live.recovery.rate-limit",
		category: "recovery",
		prompt: "读取 issue 42；如果遇到可重试的限流错误，请按工具策略重试并完成任务。",
		service: [
			{ toolName: "forge_query", input: { operation: "get", resource: "issue", id: 42 }, outcome: { kind: "throw", code: "rate_limited", message: "Retry after cooldown", details: { retryAfterSeconds: 0 } } },
			{ toolName: "forge_query", input: { operation: "get", resource: "issue", id: 42 }, outcome: { kind: "return", value: { resource: "issue", mode: "detail", item: issue(42) } } },
		],
		expectedServiceTools: ["forge_query", "forge_query"],
	},
	{
		id: "live.budget.large-list",
		category: "budget",
		prompt: "列出当前仓库最多 50 条打开的 issue，概括结果，不要在回答中复制完整正文。",
		service: [{ toolName: "forge_query", input: { operation: "list", resource: "issue", state: "open", limit: 50 }, outcome: { kind: "return", value: { resource: "issue", mode: "list", items: Array.from({ length: 50 }, (_, index) => issue(index + 1, `secret-sentinel-${index} ${"正文".repeat(20_000)}`)) } } }],
		expectedServiceTools: ["forge_query"],
		mustNotContain: ["secret-sentinel-"],
	},
	{
		id: "live.execution.sequential-writes",
		category: "execution_mode",
		prompt: "依次关闭 issue 41 和 issue 42。只使用 Forge 工具，为每次写操作提供不同且稳定的 requestId。",
		service: [
			{ toolName: "forge_issue", input: { action: "close", requestId: "close-issue-41", id: 41 }, outcome: { kind: "return", value: { id: 41, iid: 41, state: "closed" } } },
			{ toolName: "forge_issue", input: { action: "close", requestId: "close-issue-42", id: 42 }, outcome: { kind: "return", value: { id: 42, iid: 42, state: "closed" } } },
		],
		expectedServiceTools: ["forge_issue", "forge_issue"],
	},
];
