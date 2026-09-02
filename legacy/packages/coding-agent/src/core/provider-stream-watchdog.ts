import {
	type Api,
	type AssistantMessage,
	type AssistantMessageEvent,
	type AssistantMessageEventStream,
	createAssistantMessageEventStream,
	type Model,
} from "@frelion/bone-ai";

const EMPTY_USAGE = {
	input: 0,
	output: 0,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 0,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
} as const;

interface ProviderStreamWatchdogOptions {
	model: Model<Api>;
	timeoutMs: number;
	signal?: AbortSignal;
	start: (signal: AbortSignal) => AssistantMessageEventStream | Promise<AssistantMessageEventStream>;
}

function createInitialMessage(model: Model<Api>): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: EMPTY_USAGE,
		stopReason: "stop",
		timestamp: Date.now(),
	};
}

function createTerminalMessage(
	partial: AssistantMessage,
	stopReason: "aborted" | "error",
	errorMessage: string,
): AssistantMessage {
	return {
		...partial,
		content: partial.content.map((block) => {
			const clone = { ...block } as typeof block & { index?: number; partialJson?: string };
			delete clone.index;
			delete clone.partialJson;
			return clone;
		}),
		stopReason,
		errorMessage,
		timestamp: Date.now(),
	};
}

function terminalEventForMessage(message: AssistantMessage): AssistantMessageEvent {
	if (message.stopReason === "aborted" || message.stopReason === "error") {
		return { type: "error", reason: message.stopReason, error: message };
	}
	return {
		type: "done",
		reason: message.stopReason === "length" || message.stopReason === "toolUse" ? message.stopReason : "stop",
		message,
	};
}

/**
 * Enforces provider-event idleness independently of provider SDK timeout behavior.
 * The outer stream settles before the child request is aborted so synchronous abort
 * handlers from a provider cannot win the terminal-event race.
 */
export function streamProviderWithIdleTimeout(options: ProviderStreamWatchdogOptions): AssistantMessageEventStream {
	const output = createAssistantMessageEventStream();
	const providerAbortController = new AbortController();
	let sourceStream: AssistantMessageEventStream | undefined;
	let latestMessage = createInitialMessage(options.model);
	let idleTimer: ReturnType<typeof setTimeout> | undefined;
	let settled = false;

	const clearIdleTimer = (): void => {
		if (idleTimer !== undefined) {
			clearTimeout(idleTimer);
			idleTimer = undefined;
		}
	};

	const cleanup = (): void => {
		clearIdleTimer();
		options.signal?.removeEventListener("abort", onUserAbort);
	};

	const settle = (event: AssistantMessageEvent): boolean => {
		if (settled) return false;
		settled = true;
		cleanup();
		output.push(event);
		return true;
	};

	const onIdleTimeout = (): void => {
		const errorMessage = `Provider stream timed out after ${options.timeoutMs}ms of inactivity.`;
		const terminalMessage = createTerminalMessage(latestMessage, "error", errorMessage);
		if (
			settle({
				type: "error",
				reason: "error",
				error: terminalMessage,
			})
		) {
			sourceStream?.end(terminalMessage);
			providerAbortController.abort(new Error(errorMessage));
		}
	};

	const resetIdleTimer = (): void => {
		clearIdleTimer();
		if (options.timeoutMs > 0) {
			idleTimer = setTimeout(onIdleTimeout, options.timeoutMs);
		}
	};

	function onUserAbort(): void {
		const terminalMessage = createTerminalMessage(latestMessage, "aborted", "Request was aborted");
		if (
			settle({
				type: "error",
				reason: "aborted",
				error: terminalMessage,
			})
		) {
			sourceStream?.end(terminalMessage);
			providerAbortController.abort(options.signal?.reason);
		}
	}

	if (options.signal?.aborted) {
		onUserAbort();
		return output;
	}

	options.signal?.addEventListener("abort", onUserAbort, { once: true });
	resetIdleTimer();

	void (async () => {
		try {
			const source = await options.start(providerAbortController.signal);
			sourceStream = source;
			if (settled) {
				source.end();
				return;
			}

			for await (const event of source) {
				if (settled) return;
				if ("partial" in event) {
					latestMessage = event.partial;
				}
				if (event.type === "done" || event.type === "error") {
					settle(event);
					return;
				}
				output.push(event);
				resetIdleTimer();
			}

			if (!settled) {
				settle(terminalEventForMessage(await source.result()));
			}
		} catch (error) {
			if (settled) return;
			const errorMessage = error instanceof Error ? error.message : String(error);
			settle({
				type: "error",
				reason: "error",
				error: createTerminalMessage(latestMessage, "error", errorMessage),
			});
		}
	})();

	return output;
}
