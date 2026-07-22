import { fauxAssistantMessage, fauxToolCall } from "@frelion/bone-ai";
import { Type } from "typebox";
import { afterEach, describe, expect, it } from "vitest";
import { SessionManager } from "../../src/core/session-manager.ts";
import { createHarness, type Harness } from "./harness.ts";

describe("AgentSession structured questions", () => {
	const harnesses: Harness[] = [];

	afterEach(() => {
		while (harnesses.length > 0) harnesses.pop()?.cleanup();
	});

	it("pauses on ask_user_question and continues after an answer", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		await harness.session.bindExtensions({ mode: "rpc" });
		harness.setResponses([
			fauxAssistantMessage(
				fauxToolCall(
					"ask_user_question",
					{
						questions: [
							{
								question: "Which mode?",
								header: "Mode",
								options: [
									{ label: "Fast", description: "Fast path" },
									{ label: "Safe", description: "Safe path" },
								],
							},
						],
					},
					{ id: "question-tool-call" },
				),
				{ stopReason: "toolUse" },
			),
			fauxAssistantMessage("continued"),
		]);
		expect(harness.session.getActiveToolNames()).toContain("ask_user_question");

		const prompt = harness.session.prompt("choose a mode");
		await Promise.race([
			new Promise<void>((resolve) => {
				const check = () => {
					if (harness.session.questionState.status === "awaitingAnswer") resolve();
					else setTimeout(check, 0);
				};
				check();
			}),
			new Promise<never>((_, reject) =>
				setTimeout(
					() => reject(new Error(`Question did not pause; messages: ${JSON.stringify(harness.session.messages)}`)),
					1000,
				),
			),
		]);
		const state = harness.session.questionState;
		if (state.status !== "awaitingAnswer") throw new Error("Question did not pause");
		expect(state.request.toolCallId).toBe("question-tool-call");
		harness.session.answerQuestion(state.request.id, [
			{ questionIndex: 0, question: "Which mode?", kind: "option", answer: "Safe" },
		]);
		await prompt;

		expect(harness.session.questionState).toEqual({ status: "inactive" });
		expect(harness.session.messages.some((message) => message.role === "toolResult")).toBe(true);
		expect(harness.eventsOfType("question_asked")).toHaveLength(1);
		expect(harness.eventsOfType("question_answered")).toHaveLength(1);
	});

	it("restores a pending question and continues from its original tool call", async () => {
		const sessionManager = SessionManager.inMemory();
		const request = {
			id: "persisted-question",
			toolCallId: "persisted-tool",
			questions: [
				{
					question: "Which mode?",
					header: "Mode",
					options: [
						{ label: "Fast", description: "Fast path" },
						{ label: "Safe", description: "Safe path" },
					],
				},
			],
			createdAt: new Date().toISOString(),
		};
		sessionManager.appendMessage(
			fauxAssistantMessage(
				fauxToolCall("ask_user_question", { questions: request.questions }, { id: request.toolCallId }),
				{
					stopReason: "toolUse",
				},
			),
		);
		sessionManager.appendQuestionAsked(request);
		const restored = await createHarness({ sessionManager });
		harnesses.push(restored);
		expect(restored.session.questionState).toEqual({ status: "awaitingAnswer", request });
		await restored.session.bindExtensions({ mode: "rpc" });
		restored.setResponses([fauxAssistantMessage("resumed after restart")]);
		restored.session.answerQuestion(request.id, [
			{ questionIndex: 0, question: "Which mode?", kind: "option", answer: "Fast" },
		]);
		await new Promise((resolve) => setTimeout(resolve, 0));
		await restored.session.waitForIdle();

		expect(restored.session.questionState).toEqual({ status: "inactive" });
		expect(
			restored.session.messages.some(
				(message) => message.role === "toolResult" && message.toolCallId === request.toolCallId,
			),
		).toBe(true);
		expect(
			restored.session.messages.some(
				(message) =>
					message.role === "assistant" &&
					message.content.some((content) => content.type === "text" && content.text === "resumed after restart"),
			),
		).toBe(true);
		expect(restored.eventsOfType("agent_settled")).toHaveLength(1);
		expect(restored.session.isIdle).toBe(true);
	});

	it("locks ordinary input and Plan transitions while a restored question is pending", async () => {
		const sessionManager = SessionManager.inMemory();
		const request = {
			id: "locked-question",
			toolCallId: "locked-tool",
			questions: [
				{
					question: "Which mode?",
					header: "Mode",
					options: [
						{ label: "Fast", description: "Fast path" },
						{ label: "Safe", description: "Safe path" },
					],
				},
			],
			createdAt: new Date().toISOString(),
		};
		sessionManager.appendQuestionAsked(request);
		const harness = await createHarness({ sessionManager });
		harnesses.push(harness);

		await expect(harness.session.prompt("another message")).rejects.toThrow("structured question");
		await expect(harness.session.steer("another message")).rejects.toThrow("structured question");
		await expect(harness.session.followUp("another message")).rejects.toThrow("structured question");
		expect(() => harness.session.enterPlanMode()).toThrow("structured question");
	});

	it("keeps the built-in question tool when an extension registers the same name", async () => {
		const harness = await createHarness({
			extensionFactories: [
				(pi) => {
					pi.registerTool({
						name: "ask_user_question",
						label: "Shadow Question",
						description: "Extension shadow implementation",
						parameters: Type.Object({}),
						execute: async () => ({ content: [{ type: "text", text: "shadowed" }], details: {} }),
					});
				},
			],
		});
		harnesses.push(harness);

		expect(harness.session.getToolDefinition("ask_user_question")?.description).not.toContain("shadow");
		expect(
			harness.session.agent.state.tools.find((tool) => tool.name === "ask_user_question")?.description,
		).not.toContain("shadow");
	});

	it("persists an identifiable no-ui request in print mode", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(
				fauxToolCall("ask_user_question", {
					questions: [
						{
							question: "Which mode?",
							header: "Mode",
							options: [
								{ label: "Fast", description: "Fast path" },
								{ label: "Safe", description: "Safe path" },
							],
						},
					],
				}),
				{ stopReason: "toolUse" },
			),
			fauxAssistantMessage("Which mode do you prefer?"),
		]);

		await harness.session.prompt("ask me");

		const asked = harness.eventsOfType("question_asked");
		expect(asked).toHaveLength(1);
		expect(harness.eventsOfType("question_cancelled")).toEqual([
			expect.objectContaining({ requestId: asked[0].request.id, reason: "no_ui" }),
		]);
		expect(harness.eventsOfType("question_error")).toEqual([
			expect.objectContaining({ requestId: asked[0].request.id }),
		]);
		expect(harness.session.questionState).toEqual({ status: "inactive" });
		expect(harness.sessionManager.getEntries().map((entry) => entry.type)).toContain("question_asked");
		expect(harness.sessionManager.getEntries().map((entry) => entry.type)).toContain("question_cancelled");
	});
});
