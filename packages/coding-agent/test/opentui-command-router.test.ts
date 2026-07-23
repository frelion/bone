import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test, vi } from "vitest";
import type { AgentSession } from "../src/core/agent-session.ts";
import type { AgentSessionRuntime } from "../src/core/agent-session-runtime.ts";
import type { ExtensionUIV2Context } from "../src/core/extensions/ui-v2.ts";
import { OpenTUICommandRouter } from "../src/modes/interactive/opentui-command-router.ts";

function createHarness() {
	const executeBash = vi.fn(async () => ({ output: "ok", exitCode: 0, cancelled: false, truncated: false }));
	const compact = vi.fn(async () => ({}));
	const enterPlanMode = vi.fn();
	const exitPlanMode = vi.fn();
	const setSessionName = vi.fn();
	const session = {
		isBashRunning: false,
		isStreaming: false,
		isCompacting: false,
		collaborationMode: "default",
		executeBash,
		compact,
		enterPlanMode,
		exitPlanMode,
		setSessionName,
		sessionManager: {
			getSessionName: () => "Test",
			getSessionId: () => "session-id",
			getSessionFile: () => "/tmp/session.jsonl",
			getBranch: () => [],
			getCwd: () => "/tmp",
		},
	} as unknown as AgentSession;
	const runtime = { session, cwd: "/tmp", services: { agentDir: "/tmp" } } as unknown as AgentSessionRuntime;
	const createNew = vi.fn(async () => {});
	const statuses: string[] = [];
	const onQuit = vi.fn();
	const router = new OpenTUICommandRouter({
		host: { current: runtime, createNew },
		getUI: () => undefined,
		onStatus: (message) => statuses.push(message),
		onFocusConversations: vi.fn(),
		onQuit,
	});
	return { router, session, executeBash, compact, enterPlanMode, setSessionName, createNew, statuses, onQuit };
}

describe("OpenTUICommandRouter", () => {
	test("distinguishes built-ins from ordinary prompts and extension commands", async () => {
		const { router, onQuit } = createHarness();
		expect(await router.route("explain this code")).toEqual({ handled: false });
		expect(await router.route("/extension-command arg")).toEqual({ handled: false });
		expect(await router.route("/quit")).toEqual({ handled: true, kind: "command" });
		expect(onQuit).toHaveBeenCalledOnce();
	});

	test("routes stateful session commands without prompting the model", async () => {
		const { router, compact, enterPlanMode, setSessionName, createNew, statuses } = createHarness();
		await router.route("/new");
		await router.route("/compact preserve decisions");
		await router.route("/plan");
		await router.route("/name Release work");
		expect(createNew).toHaveBeenCalledOnce();
		expect(compact).toHaveBeenCalledWith("preserve decisions");
		expect(enterPlanMode).toHaveBeenCalledOnce();
		expect(setSessionName).toHaveBeenCalledWith("Release work");
		expect(statuses).toContain("Conversation compacted");
	});

	test("routes ! and !! through executeBash with context semantics", async () => {
		const { router, executeBash } = createHarness();
		expect(await router.route("! pwd")).toEqual({ handled: true, kind: "bash" });
		expect(await router.route("!! secret-command")).toEqual({ handled: true, kind: "bash" });
		expect(executeBash).toHaveBeenNthCalledWith(1, "pwd", undefined, { excludeFromContext: false });
		expect(executeBash).toHaveBeenNthCalledWith(2, "secret-command", undefined, { excludeFromContext: true });
	});

	test("completes from the fixed built-in command catalog", async () => {
		const { router } = createHarness();
		const provider = router.createAutocompleteProvider("/tmp");
		const settings = await provider.getSuggestions(["/set"], 0, 4, {
			signal: new AbortController().signal,
			force: true,
		});
		expect(settings?.items.map((item) => item.value)).toContain("settings");
		const suggestions = await provider.getSuggestions(["/mod"], 0, 4, { signal: new AbortController().signal });
		expect(suggestions?.items.some((item) => item.value === "model")).toBe(true);
		const completion = provider.applyCompletion(["/mod"], 0, 4, { value: "model", label: "model" }, "/mod");
		expect(completion.lines[0]).toBe("/model ");
	});

	test("includes extension, prompt, and skill commands in autocomplete", async () => {
		const { router, session } = createHarness();
		(session as AgentSession & { getSlashCommands: AgentSession["getSlashCommands"] }).getSlashCommands = () => [
			{
				name: "review-worktree",
				description: "Review local changes",
				source: "extension",
				sourceInfo: {
					path: "/tmp/review.ts",
					source: "extension",
					scope: "temporary",
					origin: "top-level",
				},
			},
		];
		const provider = router.createAutocompleteProvider("/tmp");
		const suggestions = await provider.getSuggestions(["/review"], 0, 7, {
			signal: new AbortController().signal,
		});
		expect(suggestions?.items.some((item) => item.value === "review-worktree")).toBe(true);
	});

	test("maintains a multi-model cycling scope", async () => {
		const first = { provider: "test", id: "first", name: "First" };
		const second = { provider: "test", id: "second", name: "Second" };
		const setScopedModels = vi.fn();
		const session = {
			scopedModels: [],
			modelRuntime: { getAvailable: async () => [first, second] },
			setScopedModels,
		} as unknown as AgentSession;
		const runtime = { session, cwd: "/tmp" } as AgentSessionRuntime;
		const select = vi
			.fn()
			.mockResolvedValueOnce("test/first")
			.mockResolvedValueOnce("test/second")
			.mockResolvedValueOnce("__done__");
		const ui = { available: true, dialogs: { select } } as unknown as ExtensionUIV2Context;
		const router = new OpenTUICommandRouter({
			host: { current: runtime, createNew: async () => {} },
			getUI: () => ui,
			onStatus: vi.fn(),
			onFocusConversations: vi.fn(),
			onQuit: vi.fn(),
		});

		await router.route("/scoped-models");
		expect(setScopedModels).toHaveBeenCalledWith([{ model: first }, { model: second }]);
	});

	test("opens the transactional settings center", async () => {
		const root = mkdtempSync(join(tmpdir(), "bone-settings-center-"));
		const replaceScope = vi.fn(async () => {});
		const session = {
			modelRuntime: {
				getModelsJson: () => ({ providers: {} }),
				reloadConfig: vi.fn(async () => {}),
			},
		} as unknown as AgentSession;
		const runtime = {
			session,
			cwd: root,
			services: {
				agentDir: root,
				settingsManager: {
					getGlobalSettings: () => ({}),
					getProjectSettings: () => ({}),
					isProjectTrusted: () => true,
					replaceScope,
					reload: vi.fn(async () => {}),
				},
			},
		} as unknown as AgentSessionRuntime;
		const select = vi
			.fn()
			.mockResolvedValueOnce("Context & Delivery")
			.mockResolvedValueOnce("hideThinkingBlock")
			.mockResolvedValueOnce("true")
			.mockResolvedValueOnce("back")
			.mockResolvedValueOnce("save");
		const ui = { available: true, dialogs: { select } } as unknown as ExtensionUIV2Context;
		const router = new OpenTUICommandRouter({
			host: { current: runtime, createNew: async () => {} },
			getUI: () => ui,
			onStatus: vi.fn(),
			onFocusConversations: vi.fn(),
			onQuit: vi.fn(),
		});

		try {
			await router.route("/settings");
			expect(replaceScope).toHaveBeenCalledWith("global", { hideThinkingBlock: true });
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	});
});
