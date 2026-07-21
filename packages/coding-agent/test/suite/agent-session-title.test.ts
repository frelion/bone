import { fauxAssistantMessage } from "@frelion/bone-ai";
import { afterEach, describe, expect, it } from "vitest";
import { createHarness, type Harness } from "./harness.ts";

describe("AgentSession title generation", () => {
	const harnesses: Harness[] = [];

	afterEach(() => {
		while (harnesses.length > 0) harnesses.pop()?.cleanup();
	});

	it("uses an isolated model request without changing conversation state", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage("I will inspect the session lifecycle."),
			fauxAssistantMessage('{"title":"Repair background session lifecycle"}'),
		]);

		await harness.session.prompt("Switching sessions loses the background stream. Please fix it.");
		const messageCount = harness.session.messages.length;
		const entryTypes = harness.sessionManager.getEntries().map((entry) => entry.type);
		const originalModel = harness.session.model;

		const result = await harness.session.generateTitle(harness.getModel());

		expect(result).toEqual({ kind: "title", title: "Repair background session lifecycle" });
		expect(harness.session.messages).toHaveLength(messageCount);
		expect(harness.sessionManager.getEntries().map((entry) => entry.type)).toEqual(entryTypes);
		expect(harness.session.model).toBe(originalModel);
		expect(harness.getPendingResponseCount()).toBe(0);
	});

	it("generates a title while the agent is processing a response", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		let releaseResponse: (() => void) | undefined;
		let signalResponseStarted: (() => void) | undefined;
		const responseStarted = new Promise<void>((resolve) => {
			signalResponseStarted = resolve;
		});
		harness.setResponses([
			() =>
				new Promise((resolve) => {
					signalResponseStarted?.();
					releaseResponse = () => resolve(fauxAssistantMessage("The agent response is complete."));
				}),
			fauxAssistantMessage('{"title":"Name during streaming"}'),
		]);

		const prompt = harness.session.prompt("Start a long-running response.");
		await responseStarted;
		expect(harness.session.isStreaming).toBe(true);

		await expect(harness.session.generateTitle(harness.getModel())).resolves.toEqual({
			kind: "title",
			title: "Name during streaming",
		});
		expect(harness.session.isStreaming).toBe(true);

		releaseResponse?.();
		await prompt;
	});

	it("keeps the session unnamed when the title model reports insufficient detail", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.setResponses([fauxAssistantMessage("Hello."), fauxAssistantMessage('{"title":null}')]);

		await harness.session.prompt("Hello");
		const result = await harness.session.generateTitle(harness.getModel());

		expect(result).toEqual({ kind: "not-ready" });
		expect(harness.sessionManager.getSessionName()).toBeUndefined();
	});

	it("keeps a model-provided not-ready message for the TUI to show", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage("Hello."),
			fauxAssistantMessage(
				'{"title":null,"message":"Describe what you want to build or fix, then try /name again."}',
			),
		]);

		await harness.session.prompt("Hello");
		const result = await harness.session.generateTitle(harness.getModel());

		expect(result).toEqual({
			kind: "not-ready",
			message: "Describe what you want to build or fix, then try /name again.",
		});
	});

	it("rejects malformed title responses", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.setResponses([fauxAssistantMessage("Tell me the error."), fauxAssistantMessage("not json")]);

		await harness.session.prompt("I need to fix a TypeScript build error.");
		const result = await harness.session.generateTitle(harness.getModel());

		expect(result).toEqual({ kind: "error", message: "Title model returned invalid JSON" });
	});
});
