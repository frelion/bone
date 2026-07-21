import { describe, expect, it, vi } from "vitest";
import type { AgentSessionRuntime } from "../src/core/agent-session-runtime.ts";
import { InteractiveMode } from "../src/modes/interactive/interactive-mode.ts";

type TestSession = {
	isCompacting: boolean;
	isStreaming: boolean;
	isBashRunning: boolean;
	prompt: (text: string, options?: unknown) => Promise<void>;
};

type SubmitContext = {
	defaultEditor: { onSubmit?: (text: string) => void };
	editor: {
		addToHistory?: (text: string) => void;
		setText: (text: string) => void;
	};
	session: TestSession;
	runtimeHost: AgentSessionRuntime;
	sessionHost: { prompt: (runtime: AgentSessionRuntime, text: string) => Promise<void> };
	flushPendingBashComponents: () => void;
	showError: (message: string) => void;
};

type ComposerState = {
	draft: string;
	compactionQueuedMessages: Array<{ text: string; mode: "steer" | "followUp" }>;
};

type ComposerContext = {
	composerStates: WeakMap<AgentSessionRuntime, ComposerState>;
	editor: { getText: () => string; setText: (text: string) => void };
	compactionQueuedMessages: ComposerState["compactionQueuedMessages"];
	updatePendingMessagesDisplay: () => void;
	flushCompactionQueue: () => Promise<void>;
};

type InteractiveModePrivate = {
	setupEditorSubmitHandler(this: SubmitContext): void;
	saveComposerState(this: ComposerContext, runtime: AgentSessionRuntime): void;
	restoreComposerState(this: ComposerContext, runtime: AgentSessionRuntime): void;
};

const interactiveModePrototype = InteractiveMode.prototype as unknown as InteractiveModePrivate;

function createRuntime(sessionOverrides: Partial<TestSession> = {}): AgentSessionRuntime {
	return {
		session: {
			isCompacting: false,
			isStreaming: false,
			isBashRunning: false,
			prompt: vi.fn(async () => {}),
			...sessionOverrides,
		},
	} as unknown as AgentSessionRuntime;
}

function createSubmitContext(runtime: AgentSessionRuntime): SubmitContext {
	return {
		defaultEditor: {},
		editor: {
			addToHistory: vi.fn(),
			setText: vi.fn(),
		},
		session: runtime.session as unknown as TestSession,
		runtimeHost: runtime,
		sessionHost: { prompt: vi.fn(async () => {}) },
		flushPendingBashComponents: vi.fn(),
		showError: vi.fn(),
	};
}

describe("InteractiveMode conversation input isolation", () => {
	it("routes a submission to the runtime that owned the composer on Enter", async () => {
		const runtimeA = createRuntime();
		const runtimeB = createRuntime();
		const context = createSubmitContext(runtimeA);
		interactiveModePrototype.setupEditorSubmitHandler.call(context);

		await context.defaultEditor.onSubmit?.(" message for A ");
		context.runtimeHost = runtimeB;

		expect(context.sessionHost.prompt).toHaveBeenCalledWith(runtimeA, "message for A");
		expect(context.editor.addToHistory).toHaveBeenCalledWith("message for A");
	});

	it("does not show a prompt failure in a different foreground conversation", async () => {
		const runtimeA = createRuntime();
		const runtimeB = createRuntime();
		let rejectPrompt: (error: Error) => void = () => {};
		const context = createSubmitContext(runtimeA);
		context.sessionHost.prompt = vi.fn(
			async () =>
				await new Promise<void>((_resolve, reject) => {
					rejectPrompt = reject;
				}),
		);
		interactiveModePrototype.setupEditorSubmitHandler.call(context);

		await context.defaultEditor.onSubmit?.("message for A");
		context.runtimeHost = runtimeB;
		rejectPrompt(new Error("A failed"));
		await vi.waitFor(() => expect(context.sessionHost.prompt).toHaveBeenCalled());

		expect(context.showError).not.toHaveBeenCalled();
	});

	it("restores drafts and compaction queues independently per runtime", () => {
		const runtimeA = createRuntime({ isCompacting: true });
		const runtimeB = createRuntime({ isCompacting: true });
		let draft = "draft A";
		const context: ComposerContext = {
			composerStates: new WeakMap(),
			editor: {
				getText: () => draft,
				setText: (text) => {
					draft = text;
				},
			},
			compactionQueuedMessages: [{ text: "queued A", mode: "steer" }],
			updatePendingMessagesDisplay: vi.fn(),
			flushCompactionQueue: vi.fn(async () => {}),
		};

		interactiveModePrototype.saveComposerState.call(context, runtimeA);
		draft = "draft B";
		context.compactionQueuedMessages = [{ text: "queued B", mode: "followUp" }];
		interactiveModePrototype.saveComposerState.call(context, runtimeB);

		interactiveModePrototype.restoreComposerState.call(context, runtimeA);
		expect(draft).toBe("draft A");
		expect(context.compactionQueuedMessages).toEqual([{ text: "queued A", mode: "steer" }]);

		interactiveModePrototype.restoreComposerState.call(context, runtimeB);
		expect(draft).toBe("draft B");
		expect(context.compactionQueuedMessages).toEqual([{ text: "queued B", mode: "followUp" }]);
	});
});
