import { describe, expect, it, vi } from "vitest";
import { InProcessSubagentRuntime } from "../../src/subagents/in-process-runtime.ts";
import type {
	SubagentHandoff,
	SubagentRunInput,
	SubagentRuntimeEvent,
	SubagentSession,
	SubagentYieldInput,
} from "../../src/subagents/types.ts";
import { SubagentRuntimeError } from "../../src/subagents/types.ts";

function deferred<T>(): {
	promise: Promise<T>;
	resolve(value: T): void;
	reject(error: Error): void;
} {
	let resolve = (_value: T) => {};
	let reject = (_error: Error) => {};
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

function handoff(summary: string): SubagentHandoff {
	return { status: "completed", summary };
}

describe("InProcessSubagentRuntime", () => {
	it("returns a handle immediately and exposes only the explicit handoff through wait", async () => {
		const result = deferred<SubagentHandoff>();
		const inputs: SubagentRunInput[] = [];
		const events: SubagentRuntimeEvent[] = [];
		const runtime = new InProcessSubagentRuntime({
			createId: () => "child-1",
			now: () => 100,
			createSession: () => ({
				run: async (input) => {
					inputs.push(input);
					return await result.promise;
				},
				abort: async () => {},
			}),
		});
		runtime.subscribe((event) => events.push(event));

		const handle = await runtime.delegate({ objective: " Inspect authentication ", label: "Auth investigator" });

		expect(handle).toMatchObject({
			id: "child-1",
			label: "Auth investigator",
			scope: "exchange",
			status: "starting",
		});
		await vi.waitFor(() => expect(runtime.get(handle.id)?.status).toBe("running"));
		expect(runtime.get(handle.id)?.status).toBe("running");
		expect(inputs).toEqual([{ kind: "delegation", text: "Inspect authentication" }]);

		result.resolve(handoff("The refresh path drops the rotated token."));
		await expect(runtime.wait(handle.id)).resolves.toEqual(handoff("The refresh path drops the rotated token."));
		expect(runtime.get(handle.id)).toMatchObject({ status: "idle", lastRunStatus: "completed" });
		expect(events.map((event) => event.type)).toEqual(["session_created", "run_started", "run_completed"]);
	});

	it("keeps the child session alive for later questions", async () => {
		const inputs: SubagentRunInput[] = [];
		let factoryCalls = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "child-2",
			createSession: () => {
				factoryCalls++;
				return {
					run: async (input) => {
						inputs.push(input);
						return handoff(input.kind === "delegation" ? "Initial conclusion" : "Follow-up answer");
					},
					abort: async () => {},
				};
			},
		});

		const handle = await runtime.delegate({
			objective: "Implement the parser",
			scope: "conversation",
		});
		await runtime.wait(handle.id);
		const answer = await runtime.ask(handle.id, "Why did you choose a streaming parser?");

		expect(answer.summary).toBe("Follow-up answer");
		expect(factoryCalls).toBe(1);
		expect(inputs).toEqual([
			{ kind: "delegation", text: "Implement the parser" },
			{ kind: "question", text: "Why did you choose a streaming parser?" },
		]);
		expect(runtime.get(handle.id)).toMatchObject({
			scope: "conversation",
			status: "idle",
			lastRunStatus: "completed",
		});
	});

	it("bounds handoffs before they reach the parent context", async () => {
		const runtime = new InProcessSubagentRuntime({
			createId: () => "bounded-child",
			createSession: () => ({
				run: async () => ({
					status: "completed",
					summary: "x".repeat(9_000),
					decisions: Array.from({ length: 30 }, (_, index) => `decision-${index}`),
				}),
				abort: async () => {},
			}),
		});

		const handle = await runtime.delegate({ objective: "Produce a large result" });
		const result = await runtime.wait(handle.id);

		expect(result.summary).toHaveLength(4_000);
		expect(result.summary.endsWith("…")).toBe(true);
		expect(result.decisions).toHaveLength(5);
	});

	it("runs independent delegated sessions concurrently", async () => {
		const pending = new Map<string, ReturnType<typeof deferred<SubagentHandoff>>>();
		let nextId = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => `child-${++nextId}`,
			createSession: ({ handle }) => ({
				run: async () => {
					const result = deferred<SubagentHandoff>();
					pending.set(handle.id, result);
					return await result.promise;
				},
				abort: async () => {},
			}),
		});

		const first = await runtime.delegate({ objective: "Inspect backend" });
		const second = await runtime.delegate({ objective: "Inspect frontend" });
		await Promise.resolve();

		expect(runtime.list().map((handle) => handle.status)).toEqual(["running", "running"]);
		pending.get(first.id)?.resolve(handoff("Backend done"));
		pending.get(second.id)?.resolve(handoff("Frontend done"));
		await expect(Promise.all([runtime.wait(first.id), runtime.wait(second.id)])).resolves.toEqual([
			handoff("Backend done"),
			handoff("Frontend done"),
		]);
	});

	it("enforces concurrent and retained child-session limits", async () => {
		const runs: Array<ReturnType<typeof deferred<SubagentHandoff>>> = [];
		let nextId = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => `limited-${++nextId}`,
			maxConcurrentChildren: 1,
			maxRetainedSessions: 1,
			createSession: () => ({
				run: async () => {
					const result = deferred<SubagentHandoff>();
					runs.push(result);
					return await result.promise;
				},
				abort: async () => {},
			}),
		});

		const first = await runtime.delegate({ objective: "First" });
		await expect(runtime.delegate({ objective: "Concurrent" })).rejects.toMatchObject({ code: "busy" });
		await vi.waitFor(() => expect(runs).toHaveLength(1));
		runs[0]?.resolve(handoff("First done"));
		await runtime.wait(first.id);
		await expect(runtime.delegate({ objective: "Retained" })).rejects.toMatchObject({ code: "busy" });
		await runtime.close(first.id);
		const second = await runtime.delegate({ objective: "After close" });
		expect(second.id).toBe("limited-2");
		await vi.waitFor(() => expect(runs).toHaveLength(2));
		runs[1]?.resolve(handoff("Second done"));
		await runtime.wait(second.id);
	});

	it("cancels only the active run and permits another question in the same session", async () => {
		const first = deferred<SubagentHandoff>();
		let aborted = false;
		let runCount = 0;
		const session: SubagentSession = {
			run: async () => {
				runCount++;
				return runCount === 1 ? await first.promise : handoff("Recovered answer");
			},
			abort: async () => {
				aborted = true;
				first.reject(new Error("aborted"));
			},
		};
		const runtime = new InProcessSubagentRuntime({
			createId: () => "child-3",
			createSession: () => session,
		});
		const handle = await runtime.delegate({ objective: "Long investigation" });
		await Promise.resolve();

		await runtime.cancel(handle.id, "Parent changed direction");

		expect(aborted).toBe(true);
		expect(runtime.get(handle.id)).toMatchObject({ status: "idle", lastRunStatus: "cancelled" });
		await expect(runtime.wait(handle.id)).rejects.toMatchObject({ code: "cancelled" });
		await expect(runtime.ask(handle.id, "Can you answer a smaller question?")).resolves.toEqual(
			handoff("Recovered answer"),
		);
	});

	it("serializes concurrent cancellation requests", async () => {
		const runGate = deferred<SubagentHandoff>();
		let abortCalls = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "cancel-race-child",
			createSession: () => ({
				run: async () => await runGate.promise,
				abort: async () => {
					abortCalls++;
					runGate.reject(new Error("aborted"));
				},
			}),
		});
		const handle = await runtime.delegate({ objective: "Long task" });
		await vi.waitFor(() => expect(runtime.get(handle.id)?.status).toBe("running"));

		const first = runtime.cancel(handle.id, "Stop");
		const second = runtime.cancel(handle.id, "Stop again");
		await Promise.all([first, second]);

		expect(abortCalls).toBe(1);
		expect(runtime.get(handle.id)).toMatchObject({ status: "idle", lastRunStatus: "cancelled" });
	});

	it("closes idempotently and rejects further questions", async () => {
		let closeCalls = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "child-4",
			createSession: () => ({
				run: async () => handoff("Done"),
				abort: async () => {},
				close: async () => {
					closeCalls++;
				},
			}),
		});
		const handle = await runtime.delegate({ objective: "One task" });
		await runtime.wait(handle.id);

		await runtime.close(handle.id);
		await runtime.close(handle.id);

		expect(closeCalls).toBe(1);
		expect(runtime.get(handle.id)?.status).toBe("closed");
		await expect(runtime.ask(handle.id, "Anything else?")).rejects.toBeInstanceOf(SubagentRuntimeError);
		await expect(runtime.ask(handle.id, "Anything else?")).rejects.toMatchObject({ code: "closed" });
	});

	it("serializes concurrent close calls and rejects runs while closing", async () => {
		const closeGate = deferred<void>();
		let closeCalls = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "closing-child",
			createSession: () => ({
				run: async () => handoff("Done"),
				abort: async () => {},
				close: async () => {
					closeCalls++;
					await closeGate.promise;
				},
			}),
		});
		const handle = await runtime.delegate({ objective: "One task" });
		await runtime.wait(handle.id);

		const firstClose = runtime.close(handle.id);
		const secondClose = runtime.close(handle.id);
		await vi.waitFor(() => expect(closeCalls).toBe(1));

		expect(runtime.get(handle.id)?.status).toBe("closing");
		expect(closeCalls).toBe(1);
		await expect(runtime.ask(handle.id, "Race the close")).rejects.toMatchObject({ code: "closed" });

		closeGate.resolve();
		await Promise.all([firstClose, secondClose]);
		expect(closeCalls).toBe(1);
		expect(runtime.get(handle.id)?.status).toBe("closed");
	});

	it("evicts the oldest closed records when the history limit is exceeded", async () => {
		let nextId = 0;
		const events: SubagentRuntimeEvent[] = [];
		const runtime = new InProcessSubagentRuntime({
			createId: () => `closed-${++nextId}`,
			now: (() => {
				let current = 10;
				return () => current++;
			})(),
			maxClosedSessions: 2,
			createSession: () => ({
				run: async () => handoff("Done"),
				abort: async () => {},
			}),
		});
		runtime.subscribe((event) => events.push(event));

		const first = await runtime.delegate({ objective: "First" });
		await runtime.wait(first.id);
		const second = await runtime.delegate({ objective: "Second" });
		await runtime.wait(second.id);
		const third = await runtime.delegate({ objective: "Third" });
		await runtime.wait(third.id);
		await runtime.close(third.id);
		await runtime.close(first.id);
		await runtime.close(second.id);

		expect(runtime.get(first.id)).toBeUndefined();
		expect(runtime.list().map((handle) => handle.id)).toEqual([second.id, third.id]);
		expect(events.filter((event) => event.type === "session_evicted").map((event) => event.handle.id)).toEqual([
			first.id,
		]);
		expect(events.map((event) => event.type).lastIndexOf("session_evicted")).toBeGreaterThan(
			events.map((event) => event.type).lastIndexOf("session_closed"),
		);
	});

	it("evicts a closed session immediately when maxClosedSessions is zero", async () => {
		const events: SubagentRuntimeEvent[] = [];
		const runtime = new InProcessSubagentRuntime({
			createId: () => "no-closed-history",
			maxClosedSessions: 0,
			createSession: () => ({
				run: async () => handoff("Done"),
				abort: async () => {},
			}),
		});
		runtime.subscribe((event) => events.push(event));
		const handle = await runtime.delegate({ objective: "Ephemeral" });
		await runtime.wait(handle.id);

		await runtime.close(handle.id);

		expect(runtime.get(handle.id)).toBeUndefined();
		expect(runtime.list()).toEqual([]);
		expect(events.slice(-2).map((event) => event.type)).toEqual(["session_closed", "session_evicted"]);
		expect(events.at(-1)).toMatchObject({
			type: "session_evicted",
			handle: { id: handle.id, status: "closed" },
		});
	});

	it("validates maxClosedSessions independently from active-session limits", () => {
		const createSession = () => ({
			run: async () => handoff("Done"),
			abort: async () => {},
		});

		expect(() => new InProcessSubagentRuntime({ createSession, maxClosedSessions: -1 })).toThrow(
			"maxClosedSessions must be a non-negative integer",
		);
		expect(() => new InProcessSubagentRuntime({ createSession, maxClosedSessions: 1.5 })).toThrow(
			"maxClosedSessions must be a non-negative integer",
		);
		expect(() => new InProcessSubagentRuntime({ createSession, maxClosedSessions: 0 })).not.toThrow();
	});

	it("cancels session creation without waiting for an uncooperative factory and disposes a late session", async () => {
		const factoryGate = deferred<SubagentSession>();
		let factorySignal: AbortSignal | undefined;
		let lateCloseCalls = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "starting-child",
			createSession: ({ signal }) => {
				factorySignal = signal;
				return factoryGate.promise;
			},
		});
		const handle = await runtime.delegate({ objective: "Slow startup" });
		await Promise.resolve();

		await runtime.cancel(handle.id, "Stop startup");

		expect(factorySignal?.aborted).toBe(true);
		expect(runtime.get(handle.id)).toMatchObject({ status: "idle", lastRunStatus: "cancelled" });
		factoryGate.resolve({
			run: async () => handoff("Too late"),
			abort: async () => {},
			close: async () => {
				lateCloseCalls++;
			},
		});
		await Promise.resolve();
		await Promise.resolve();
		expect(lateCloseCalls).toBe(1);
	});

	it("propagates an AbortSignal from wait and leaves the session reusable", async () => {
		const runGate = deferred<SubagentHandoff>();
		let abortCalls = 0;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "signal-child",
			createSession: () => ({
				run: async (_input, signal) => {
					return await new Promise<SubagentHandoff>((resolve, reject) => {
						const onAbort = () => reject(new Error("run aborted"));
						signal?.addEventListener("abort", onAbort, { once: true });
						runGate.promise.then(resolve, reject);
					});
				},
				abort: async () => {
					abortCalls++;
				},
			}),
		});
		const handle = await runtime.delegate({ objective: "Long task" });
		await Promise.resolve();
		const controller = new AbortController();
		const waiting = runtime.wait(handle.id, controller.signal);

		controller.abort("Parent stopped");

		await expect(waiting).rejects.toMatchObject({ code: "cancelled", message: "Parent stopped" });
		await vi.waitFor(() => {
			expect(runtime.get(handle.id)?.lastRunStatus).toBe("cancelled");
		});
		expect(abortCalls).toBe(1);
	});

	it("closes all sessions in a selected scope", async () => {
		let nextId = 0;
		const closed: string[] = [];
		const runtime = new InProcessSubagentRuntime({
			createId: () => `scope-${++nextId}`,
			createSession: ({ handle }) => ({
				run: async () => handoff("Done"),
				abort: async () => {},
				close: async () => {
					closed.push(handle.id);
				},
			}),
		});
		const exchange = await runtime.delegate({ objective: "Short", scope: "exchange" });
		const conversation = await runtime.delegate({ objective: "Long-lived", scope: "conversation" });
		await Promise.all([runtime.wait(exchange.id), runtime.wait(conversation.id)]);

		await runtime.closeAll("exchange");

		expect(closed).toEqual([exchange.id]);
		expect(runtime.get(exchange.id)?.status).toBe("closed");
		expect(runtime.get(conversation.id)?.status).toBe("idle");
	});

	it("reports a failed handoff as a failed run while preserving its diagnostics", async () => {
		const events: SubagentRuntimeEvent[] = [];
		const runtime = new InProcessSubagentRuntime({
			createId: () => "failed-child",
			createSession: () => ({
				run: async () => ({ status: "failed", summary: "Provider unavailable" }),
				abort: async () => {},
			}),
		});
		runtime.subscribe((event) => events.push(event));
		const handle = await runtime.delegate({ objective: "Try work" });

		await expect(runtime.wait(handle.id)).resolves.toEqual({
			status: "failed",
			summary: "Provider unavailable",
		});
		expect(runtime.get(handle.id)).toMatchObject({ status: "failed", lastRunStatus: "failed" });
		expect(events.map((event) => event.type)).toEqual(["session_created", "run_started", "run_failed"]);
	});

	it("publishes ordered child yields and atomically drains the unread mailbox", async () => {
		const runGate = deferred<SubagentHandoff>();
		const events: SubagentRuntimeEvent[] = [];
		let publishYield!: (input: SubagentYieldInput) => void;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "yield-child",
			now: (() => {
				let current = 100;
				return () => current++;
			})(),
			createSession: (context) => {
				publishYield = context.publishYield;
				return {
					run: async () => await runGate.promise,
					abort: async () => {},
				};
			},
		});
		runtime.subscribe((event) => events.push(event));
		const handle = await runtime.delegate({ objective: "Stream discoveries" });
		await vi.waitFor(() => expect(runtime.get(handle.id)?.status).toBe("running"));

		publishYield({ kind: "progress", message: "Indexed files" });
		await Promise.all([
			Promise.resolve().then(() => publishYield({ kind: "finding", message: "Found the cause" })),
			Promise.resolve().then(() => publishYield({ kind: "risk", message: "Migration risk" })),
		]);

		const drained = runtime.drainYields(handle.id);
		expect(drained.map(({ sequence, kind, message }) => ({ sequence, kind, message }))).toEqual([
			{ sequence: 1, kind: "progress", message: "Indexed files" },
			{ sequence: 2, kind: "finding", message: "Found the cause" },
			{ sequence: 3, kind: "risk", message: "Migration risk" },
		]);
		expect(drained.every((item) => item.agentRef === handle.id)).toBe(true);
		expect(drained.map((item) => item.createdAt)).toEqual([102, 103, 104]);
		expect(runtime.drainYields(handle.id)).toEqual([]);
		expect(events.filter((event) => event.type === "yield_published").map((event) => event.yield.sequence)).toEqual([
			1, 2, 3,
		]);

		runGate.resolve(handoff("Done"));
		await runtime.wait(handle.id);
	});

	it("bounds yield text, references, and mailbox size while retaining the newest updates", async () => {
		const runGate = deferred<SubagentHandoff>();
		let publishYield!: (input: SubagentYieldInput) => void;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "bounded-yield-child",
			createSession: ({ publishYield: publish }) => {
				publishYield = publish;
				return {
					run: async () => await runGate.promise,
					abort: async () => {},
				};
			},
		});
		const handle = await runtime.delegate({ objective: "Publish lots" });
		await vi.waitFor(() => expect(runtime.get(handle.id)?.status).toBe("running"));

		for (let index = 1; index <= 25; index++) {
			publishYield({
				kind: "finding",
				message: `${index}:${"x".repeat(1_100)}`,
				artifactRefs: Array.from({ length: 8 }, (_, refIndex) => `${refIndex}:${"r".repeat(300)}`),
			});
		}

		const drained = runtime.drainYields(handle.id);
		expect(drained).toHaveLength(20);
		expect(drained[0]?.sequence).toBe(6);
		expect(drained.at(-1)?.sequence).toBe(25);
		expect(drained.every((item) => item.message.length === 1_000 && item.message.endsWith("…"))).toBe(true);
		expect(drained.every((item) => item.artifactRefs?.length === 5)).toBe(true);
		expect(drained.every((item) => item.artifactRefs?.every((ref) => ref.length === 250))).toBe(true);

		runGate.resolve(handoff("Done"));
		await runtime.wait(handle.id);
	});

	it("keeps unread yields drainable after close and ignores late publishers", async () => {
		let publishYield!: (input: { kind: "progress"; message: string }) => void;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "closed-yield-child",
			createSession: ({ publishYield: publish }) => {
				publishYield = publish;
				return {
					run: async () => {
						publish({ kind: "progress", message: "Before close" });
						return handoff("Done");
					},
					abort: async () => {},
				};
			},
		});
		const handle = await runtime.delegate({ objective: "Close with unread mail" });
		await runtime.wait(handle.id);
		await runtime.close(handle.id);

		publishYield({ kind: "progress", message: "Too late" });

		expect(runtime.drainYields(handle.id).map((item) => item.message)).toEqual(["Before close"]);
		expect(runtime.drainYields(handle.id)).toEqual([]);
	});

	it("installs the busy marker before synchronous event listeners can re-enter", async () => {
		const runtime = new InProcessSubagentRuntime({
			createId: () => "reentrant-child",
			createSession: () => ({
				run: async (input) => handoff(input.text),
				abort: async () => {},
			}),
		});
		const handle = await runtime.delegate({ objective: "Initial" });
		await runtime.wait(handle.id);
		let nested: Promise<SubagentHandoff> | undefined;
		runtime.subscribe((event) => {
			if (event.type === "run_started" && event.input.text === "Outer") {
				nested = runtime.ask(handle.id, "Nested");
			}
		});

		await expect(runtime.ask(handle.id, "Outer")).resolves.toEqual(handoff("Outer"));
		await expect(nested).rejects.toMatchObject({ code: "busy" });
	});

	it("rejects empty tasks, duplicate IDs, and unknown handles", async () => {
		const runtime = new InProcessSubagentRuntime({
			createId: () => "fixed",
			createSession: () => ({
				run: async () => handoff("Done"),
				abort: async () => {},
			}),
		});

		await expect(runtime.delegate({ objective: " " })).rejects.toMatchObject({ code: "invalid_argument" });
		const handle = await runtime.delegate({ objective: "First" });
		await runtime.wait(handle.id);
		await expect(runtime.delegate({ objective: "Second" })).rejects.toMatchObject({
			code: "invalid_argument",
		});
		await expect(runtime.wait("missing")).rejects.toMatchObject({ code: "unknown_agent" });
	});
});
