import type { ForgeEvalCase } from "./types.ts";

const issue = (number: number, body = "Focused issue body") => ({
	id: number,
	iid: number,
	title: number === 42 ? "Login fails" : `Issue ${number}`,
	state: "open",
	body,
	html_url: `https://github.com/example/repo/issues/${number}`,
});

export const forgeEvalCases: readonly ForgeEvalCase[] = [
	{
		id: "query.issue.list-success",
		category: "query",
		prompt: "找出当前仓库打开的 issue，并告诉我找到的标题。",
		steps: [
			{ toolCalls: [{ name: "forge_query", args: { operation: "list", resource: "issue", state: "open" } }] },
			{ text: "找到了一个打开的 issue：Login fails。" },
		],
		service: [
			{
				toolName: "forge_query",
				input: { operation: "list", resource: "issue", state: "open" },
				outcome: { kind: "return", value: { resource: "issue", mode: "list", items: [issue(42)] } },
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 1,
			expectedServiceTools: ["forge_query"],
			expectedContextIncludesToolResult: true,
			assertions: ["first tool call passes schema", "model receives bounded tool result before final response"],
		},
	},
	{
		id: "query.validation-correction",
		category: "recovery",
		prompt: "读取 issue 42。",
		steps: [
			{
				toolCalls: [
					{
						name: "forge_query",
						args: { operation: "get", resource: "issue", id: "", ids: [0], unused: "placeholder" },
					},
				],
			},
			{ toolCalls: [{ name: "forge_query", args: { operation: "get", resource: "issue", id: 42 } }] },
			{ text: "issue 42 的标题是 Login fails。" },
		],
		service: [
			{
				toolName: "forge_query",
				input: { operation: "get", resource: "issue", id: 42 },
				outcome: { kind: "return", value: { resource: "issue", mode: "detail", item: issue(42) } },
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 1,
			expectedErrors: ["Validation failed for tool"],
			expectedContextIncludesToolResult: true,
			assertions: ["invalid placeholder payload never reaches service", "model corrects operation-specific arguments"],
		},
	},
	{
		id: "mutation.issue.compact-receipt",
		category: "mutation",
		prompt: "关闭 issue 42。",
		steps: [
			{
				toolCalls: [
					{
						name: "forge_issue",
						args: { action: "close", requestId: "close-42", issueNumber: 42 },
					},
				],
			},
			{ text: "issue 42 已关闭。" },
		],
		service: [
			{
				toolName: "forge_issue",
				input: { action: "close", requestId: "close-42", id: 42 },
				outcome: {
					kind: "return",
					value: {
						id: 42,
						iid: 42,
						state: "closed",
						html_url: "https://github.com/example/repo/issues/42",
						providerRawPayload: "must not survive projection",
					},
				},
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 1,
			expectedServiceTools: ["forge_issue"],
			maxToolResultBytes: 16 * 1024,
			expectedContextIncludesToolResult: true,
			mustNotContain: ["providerRawPayload"],
			assertions: ["issueNumber is normalized to service id", "mutation returns compact receipt without raw provider fields"],
		},
	},
	{
		id: "query.not-found.corrected-id",
		category: "recovery",
		prompt: "找到正确的登录 issue。",
		steps: [
			{ toolCalls: [{ name: "forge_query", args: { operation: "get", resource: "issue", id: 41 } }] },
			{ toolCalls: [{ name: "forge_query", args: { operation: "get", resource: "issue", id: 41 } }] },
			{ toolCalls: [{ name: "forge_query", args: { operation: "get", resource: "issue", id: 42 } }] },
			{ text: "找到了 issue 42：Login fails。" },
		],
		service: [
			{
				toolName: "forge_query",
				input: { operation: "get", resource: "issue", id: 41 },
				outcome: { kind: "throw", code: "not_found", message: "Issue 41 was not found" },
			},
			{
				toolName: "forge_query",
				input: { operation: "get", resource: "issue", id: 42 },
				outcome: { kind: "return", value: { resource: "issue", mode: "detail", item: issue(42) } },
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 2,
			expectedErrors: ["not_found", "duplicate_failed_call"],
			expectedDuplicateFailures: 1,
			assertions: ["unchanged non-retryable call is blocked", "changed id is allowed to continue"],
		},
	},
	{
		id: "query.rate-limit.bounded-retry",
		category: "recovery",
		prompt: "读取 issue 42，遇到限流时按策略重试。",
		steps: [
			{ toolCalls: [{ name: "forge_query", args: { operation: "get", resource: "issue", id: 42 } }] },
			{ toolCalls: [{ name: "forge_query", args: { operation: "get", resource: "issue", id: 42 } }] },
			{ text: "issue 42 的标题是 Login fails。" },
		],
		service: [
			{
				toolName: "forge_query",
				input: { operation: "get", resource: "issue", id: 42 },
				outcome: { kind: "throw", code: "rate_limited", message: "Retry after cooldown", details: { retryAfterSeconds: 1 } },
			},
			{
				toolName: "forge_query",
				input: { operation: "get", resource: "issue", id: 42 },
				outcome: { kind: "return", value: { resource: "issue", mode: "detail", item: issue(42) } },
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 2,
			expectedErrors: ["rate_limited"],
			assertions: ["retryable error is retried within the declared attempt limit"],
		},
	},
	{
		id: "query.large-result.output-budget",
		category: "budget",
		prompt: "列出所有打开的 issue，但不要把整个 provider 响应塞进上下文。",
		steps: [
			{ toolCalls: [{ name: "forge_query", args: { operation: "list", resource: "issue", state: "open", limit: 50 } }] },
			{ text: "结果已按工具预算截断，请按需读取单条详情。" },
		],
		service: [
			{
				toolName: "forge_query",
				input: { operation: "list", resource: "issue", state: "open", limit: 50 },
				outcome: {
					kind: "return",
					value: {
						resource: "issue",
						mode: "list",
						items: Array.from({ length: 50 }, (_, index) => issue(index + 1, `secret-sentinel-${index} ${"正文".repeat(20_000)}`)),
					},
				},
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 1,
			maxToolResultBytes: 64 * 1024,
			maxContextBytes: 128 * 1024,
			mustNotContain: ["secret-sentinel-"],
			assertions: ["large provider payload is bounded before the next model turn"],
		},
	},
	{
		id: "mutation.writes.execute-sequentially",
		category: "execution_mode",
		prompt: "依次关闭 issue 41 和 issue 42。",
		steps: [
			{
				toolCalls: [
					{ name: "forge_issue", args: { action: "close", requestId: "close-41", issueNumber: 41 } },
					{ name: "forge_issue", args: { action: "close", requestId: "close-42", issueNumber: 42 } },
				],
			},
			{ text: "两个 issue 都已关闭。" },
		],
		service: [
			{
				toolName: "forge_issue",
				input: { action: "close", requestId: "close-41", id: 41 },
				outcome: { kind: "return", value: { id: 41, iid: 41, state: "closed" } },
			},
			{
				toolName: "forge_issue",
				input: { action: "close", requestId: "close-42", id: 42 },
				outcome: { kind: "return", value: { id: 42, iid: 42, state: "closed" } },
			},
		],
		expectations: {
			mustComplete: true,
			expectedServiceCalls: 2,
			expectedServiceTools: ["forge_issue", "forge_issue"],
			assertions: ["write tool calls execute in source order"],
		},
	},
];
