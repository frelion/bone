import {
	type AssistantMessage,
	type AssistantMessageEvent,
	createAssistantMessageEventStream,
	type Model,
} from "@frelion/bone-ai";
import { afterEach, describe, expect, it, vi } from "vitest";
import { streamProviderWithIdleTimeout } from "../src/core/provider-stream-watchdog.ts";

const model: Model<"openai-responses"> = {
	id: "watchdog-model",
	name: "Watchdog Model",
	api: "openai-responses",
	provider: "watchdog-provider",
	baseUrl: "https://watchdog.invalid/v1",
	reasoning: false,
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 128_000,
	maxTokens: 4096,
};

function message(text = ""): AssistantMessage {
	return {
		role: "assistant",
		content: text ? [{ type: "text", text }] : [],
		api: model.api,
		provider: model.provider,
		model: model.id,
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
}

async function collect(stream: ReturnType<typeof streamProviderWithIdleTimeout>): Promise<AssistantMessageEvent[]> {
	const events: AssistantMessageEvent[] = [];
	for await (const event of stream) events.push(event);
	return events;
}

afterEach(() => {
	vi.useRealTimers();
});

describe("provider stream idle watchdog", () => {
	it("settles with an error and aborts a provider that emits no events", async () => {
		vi.useFakeTimers();
		const source = createAssistantMessageEventStream();
		const endSource = vi.spyOn(source, "end");
		let providerSignal: AbortSignal | undefined;
		const stream = streamProviderWithIdleTimeout({
			model,
			timeoutMs: 100,
			start: (signal) => {
				providerSignal = signal;
				return source;
			},
		});

		await vi.advanceTimersByTimeAsync(100);
		const result = await stream.result();

		expect(result.stopReason).toBe("error");
		expect(result.errorMessage).toContain("100ms of inactivity");
		expect(providerSignal?.aborted).toBe(true);
		expect(endSource).toHaveBeenCalledOnce();
	});

	it("resets the timeout after every provider event", async () => {
		vi.useFakeTimers();
		const source = createAssistantMessageEventStream();
		const stream = streamProviderWithIdleTimeout({ model, timeoutMs: 100, start: () => source });
		let settled = false;
		void stream.result().then(() => {
			settled = true;
		});

		await vi.advanceTimersByTimeAsync(90);
		source.push({ type: "start", partial: message() });
		await vi.advanceTimersByTimeAsync(90);
		source.push({ type: "text_delta", contentIndex: 0, delta: "ok", partial: message("ok") });
		await vi.advanceTimersByTimeAsync(90);
		expect(settled).toBe(false);

		source.push({ type: "done", reason: "stop", message: message("ok") });
		expect((await stream.result()).stopReason).toBe("stop");
	});

	it("disables the idle timeout when configured with zero", async () => {
		vi.useFakeTimers();
		const source = createAssistantMessageEventStream();
		const stream = streamProviderWithIdleTimeout({ model, timeoutMs: 0, start: () => source });
		let settled = false;
		void stream.result().then(() => {
			settled = true;
		});

		await vi.advanceTimersByTimeAsync(1_000_000);
		expect(settled).toBe(false);

		source.push({ type: "done", reason: "stop", message: message("ok") });
		expect((await stream.result()).stopReason).toBe("stop");
	});

	it("settles as aborted immediately when the user aborts", async () => {
		const source = createAssistantMessageEventStream();
		const endSource = vi.spyOn(source, "end");
		const userAbortController = new AbortController();
		let providerSignal: AbortSignal | undefined;
		const stream = streamProviderWithIdleTimeout({
			model,
			timeoutMs: 10_000,
			signal: userAbortController.signal,
			start: (signal) => {
				providerSignal = signal;
				return source;
			},
		});
		const iterator = stream[Symbol.asyncIterator]();
		const startEventPromise = iterator.next();
		source.push({ type: "start", partial: message("partial") });
		expect((await startEventPromise).value?.type).toBe("start");

		userAbortController.abort();
		const result = await stream.result();
		const terminalEvent = await iterator.next();

		expect(result.stopReason).toBe("aborted");
		expect(result.errorMessage).toBe("Request was aborted");
		expect(result.content).toEqual([{ type: "text", text: "partial" }]);
		expect(providerSignal?.aborted).toBe(true);
		expect(endSource).toHaveBeenCalledOnce();
		expect(terminalEvent.value?.type).toBe("error");
		expect((await iterator.next()).done).toBe(true);
	});

	it("emits exactly one terminal event when provider abort handling races with timeout", async () => {
		vi.useFakeTimers();
		const source = createAssistantMessageEventStream();
		const stream = streamProviderWithIdleTimeout({
			model,
			timeoutMs: 100,
			start: (signal) => {
				signal.addEventListener("abort", () => {
					const aborted = { ...message(), stopReason: "aborted" as const, errorMessage: "provider aborted" };
					source.push({ type: "error", reason: "aborted", error: aborted });
				});
				return source;
			},
		});
		const eventsPromise = collect(stream);

		await vi.advanceTimersByTimeAsync(100);
		const events = await eventsPromise;

		expect(events).toHaveLength(1);
		expect(events[0]?.type).toBe("error");
		expect(events[0]?.type === "error" ? events[0].reason : undefined).toBe("error");
	});

	it("does not start the provider when the user signal is already aborted", async () => {
		const userAbortController = new AbortController();
		userAbortController.abort();
		const start = vi.fn(() => createAssistantMessageEventStream());
		const stream = streamProviderWithIdleTimeout({
			model,
			timeoutMs: 100,
			signal: userAbortController.signal,
			start,
		});

		expect((await stream.result()).stopReason).toBe("aborted");
		expect(start).not.toHaveBeenCalled();
	});
});
