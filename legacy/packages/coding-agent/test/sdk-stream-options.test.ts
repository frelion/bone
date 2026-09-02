import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	type Api,
	type AssistantMessage,
	createAssistantMessageEventStream,
	type Model,
	type SimpleStreamOptions,
} from "@frelion/bone-ai";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuthStorage } from "../src/core/auth-storage.ts";
import { createAgentSession } from "../src/core/sdk.ts";
import { SessionManager } from "../src/core/session-manager.ts";
import { SettingsManager } from "../src/core/settings-manager.ts";

import { createModelRegistry, getModelRuntime } from "./model-runtime-test-utils.ts";

describe("createAgentSession stream options", () => {
	let tempDir: string;
	let cwd: string;
	let agentDir: string;

	beforeEach(() => {
		tempDir = mkdtempSync(join(tmpdir(), "pi-sdk-stream-options-"));
		cwd = join(tempDir, "project");
		agentDir = join(tempDir, "agent");
		mkdirSync(cwd, { recursive: true });
		mkdirSync(agentDir, { recursive: true });
	});

	afterEach(() => {
		vi.useRealTimers();
		if (tempDir) {
			rmSync(tempDir, { recursive: true, force: true });
		}
	});

	function createModel(api: Api): Model<Api> {
		return {
			id: "capture-model",
			name: "Capture Model",
			api,
			provider: "capture-provider",
			baseUrl: "https://capture.invalid/v1",
			reasoning: false,
			input: ["text"],
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
			contextWindow: 128000,
			maxTokens: 4096,
			headers: { "x-model": "model" },
		};
	}

	function createDoneStream(api: Api) {
		const stream = createAssistantMessageEventStream();
		const message: AssistantMessage = {
			role: "assistant",
			content: [{ type: "text", text: "ok" }],
			api,
			provider: "capture-provider",
			model: "capture-model",
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			stopReason: "stop",
			timestamp: Date.now(),
		};
		stream.end(message);
		return stream;
	}

	async function captureStreamOptions(
		api: Api,
		settings: { httpIdleTimeoutMs?: number; websocketConnectTimeoutMs?: number },
		requestOptions: SimpleStreamOptions = {},
	): Promise<SimpleStreamOptions | undefined> {
		const model = createModel(api);
		const settingsManager = SettingsManager.inMemory(settings);

		const authStorage = AuthStorage.create(join(agentDir, "auth.json"));
		await authStorage.modify(model.provider, async () => ({ type: "api_key", key: "test-api-key" }));
		const modelRegistry = await createModelRegistry(authStorage, join(agentDir, "models.json"));
		let capturedOptions: SimpleStreamOptions | undefined;

		modelRegistry.registerProvider(model.provider, {
			api,
			headers: { "x-provider": "provider" },
			streamSimple: (_model, _context, providerOptions) => {
				capturedOptions = providerOptions;
				return createDoneStream(api);
			},
		});

		const modelRuntime = getModelRuntime(modelRegistry);
		const sessionManager = SessionManager.inMemory(cwd);
		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model,
			modelRuntime,
			settingsManager,
			sessionManager,
		});

		try {
			const stream = await session.agent.streamFn(model, { messages: [] }, requestOptions);
			await stream.result();
			return capturedOptions;
		} finally {
			session.dispose();
			modelRegistry.unregisterProvider(model.provider);
		}
	}

	async function createHangingProviderSession(retryEnabled = false) {
		const model = createModel("openai-responses");
		const settingsManager = SettingsManager.inMemory({
			httpIdleTimeoutMs: 100,
			retry: {
				enabled: retryEnabled,
				maxRetries: retryEnabled ? 1 : undefined,
				baseDelayMs: retryEnabled ? 1 : undefined,
				provider: { timeoutMs: 10_000 },
			},
		});
		const authStorage = AuthStorage.create(join(agentDir, "auth.json"));
		await authStorage.modify(model.provider, async () => ({ type: "api_key", key: "test-api-key" }));
		const modelRegistry = await createModelRegistry(authStorage, join(agentDir, "models.json"));
		let capturedOptions: SimpleStreamOptions | undefined;
		let providerSignal: AbortSignal | undefined;
		let providerCalls = 0;
		let notifyProviderStarted = (): void => {};
		const providerStarted = new Promise<void>((resolve) => {
			notifyProviderStarted = resolve;
		});

		modelRegistry.registerProvider(model.provider, {
			api: model.api,
			models: [model],
			streamSimple: (_model, _context, providerOptions) => {
				providerCalls += 1;
				capturedOptions = providerOptions;
				providerSignal = providerOptions?.signal;
				notifyProviderStarted();
				return createAssistantMessageEventStream();
			},
		});

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model,
			modelRuntime: getModelRuntime(modelRegistry),
			settingsManager,
			sessionManager: SessionManager.inMemory(cwd),
		});

		return {
			model,
			session,
			providerStarted,
			getCapturedOptions: () => capturedOptions,
			getProviderSignal: () => providerSignal,
			getProviderCalls: () => providerCalls,
			cleanup: () => {
				session.dispose();
				modelRegistry.unregisterProvider(model.provider);
			},
		};
	}

	it("forwards httpIdleTimeoutMs as timeoutMs for OpenAI Codex", async () => {
		const options = await captureStreamOptions("openai-codex-responses", { httpIdleTimeoutMs: 1234 });

		expect(options?.timeoutMs).toBe(1234);
	});

	it("defaults timeoutMs from httpIdleTimeoutMs for all providers", async () => {
		const options = await captureStreamOptions("openai-completions", { httpIdleTimeoutMs: 1234 });

		expect(options?.timeoutMs).toBe(1234);
	});

	it("lets request timeoutMs override httpIdleTimeoutMs for OpenAI Codex", async () => {
		const options = await captureStreamOptions(
			"openai-codex-responses",
			{ httpIdleTimeoutMs: 1234 },
			{ timeoutMs: 0 },
		);

		expect(options?.timeoutMs).toBe(0);
	});

	it("forwards websocketConnectTimeoutMs from settings", async () => {
		const options = await captureStreamOptions("openai-codex-responses", { websocketConnectTimeoutMs: 1234 });

		expect(options?.websocketConnectTimeoutMs).toBe(1234);
	});

	it("lets request websocketConnectTimeoutMs override settings", async () => {
		const options = await captureStreamOptions(
			"openai-codex-responses",
			{ websocketConnectTimeoutMs: 1234 },
			{ websocketConnectTimeoutMs: 0 },
		);

		expect(options?.websocketConnectTimeoutMs).toBe(0);
	});

	it("keeps stream idleness independent from a request timeout override", async () => {
		vi.useFakeTimers();
		const created = await createHangingProviderSession();

		try {
			const stream = await created.session.agent.streamFn(created.model, { messages: [] }, { timeoutMs: 20_000 });
			await created.providerStarted;
			await vi.advanceTimersByTimeAsync(100);

			expect((await stream.result()).stopReason).toBe("error");
			expect(created.getCapturedOptions()?.timeoutMs).toBe(20_000);
			expect(created.getProviderSignal()?.aborted).toBe(true);
		} finally {
			created.cleanup();
		}
	});

	it("settles the agent session when a provider never emits an event", async () => {
		vi.useFakeTimers();
		const created = await createHangingProviderSession();
		const events: string[] = [];
		const unsubscribe = created.session.subscribe((event) => events.push(event.type));

		try {
			const promptPromise = created.session.prompt("hello");
			await created.providerStarted;
			await vi.advanceTimersByTimeAsync(100);
			await promptPromise;

			expect(events).toContain("agent_start");
			expect(events.at(-1)).toBe("agent_settled");
			expect(created.session.messages.at(-1)).toMatchObject({
				role: "assistant",
				stopReason: "error",
				errorMessage: "Provider stream timed out after 100ms of inactivity.",
			});
			expect(created.getCapturedOptions()?.timeoutMs).toBe(10_000);
			expect(created.getProviderSignal()?.aborted).toBe(true);
		} finally {
			unsubscribe();
			created.cleanup();
		}
	});

	it("settles once after repeated stream timeouts exhaust automatic retries", async () => {
		vi.useFakeTimers();
		const created = await createHangingProviderSession(true);
		const events: string[] = [];
		const unsubscribe = created.session.subscribe((event) => events.push(event.type));

		try {
			const promptPromise = created.session.prompt("hello");
			await created.providerStarted;
			await vi.runAllTimersAsync();
			await promptPromise;

			expect(created.getProviderCalls()).toBe(2);
			expect(events.filter((type) => type === "agent_end")).toHaveLength(2);
			expect(events.filter((type) => type === "agent_settled")).toHaveLength(1);
			expect(events.at(-1)).toBe("agent_settled");
		} finally {
			unsubscribe();
			created.cleanup();
		}
	});
});
