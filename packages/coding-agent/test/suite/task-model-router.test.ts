import { afterEach, describe, expect, it } from "vitest";
import { resolveTaskModel } from "../../src/core/task-model-router.ts";
import { createHarness, type Harness } from "./harness.ts";

describe("task model router", () => {
	const harnesses: Harness[] = [];

	afterEach(() => {
		while (harnesses.length > 0) harnesses.pop()?.cleanup();
	});

	it("inherits the conversation model when title routing is not configured", async () => {
		const harness = await createHarness();
		harnesses.push(harness);

		const resolved = await resolveTaskModel("title", {
			conversationModel: harness.session.model,
			taskModel: harness.settingsManager.getTaskModel("title"),
			modelRuntime: harness.session.modelRuntime,
		});

		expect(resolved).toEqual({ model: harness.getModel(), source: "conversation" });
	});

	it("uses an explicit title model without changing the conversation model", async () => {
		const harness = await createHarness({
			models: [
				{ id: "chat", name: "Chat", reasoning: false },
				{ id: "title", name: "Title", reasoning: false },
			],
		});
		harnesses.push(harness);
		const chatModel = harness.getModel("chat")!;
		const titleModel = harness.getModel("title")!;
		harness.settingsManager.setTaskModel("title", {
			providerId: titleModel.provider,
			modelId: titleModel.id,
		});
		expect(harness.settingsManager.getGlobalSettings().taskModels).toEqual({
			title: { providerId: titleModel.provider, modelId: titleModel.id },
		});

		const resolved = await resolveTaskModel("title", {
			conversationModel: chatModel,
			taskModel: harness.settingsManager.getTaskModel("title"),
			modelRuntime: harness.session.modelRuntime,
		});

		expect(resolved).toEqual({ model: titleModel, source: "task-binding" });
		expect(harness.session.model).toBe(chatModel);
		expect(harness.settingsManager.getDefaultModel()).not.toBe(titleModel.id);
	});

	it("does not silently fall back when the configured title model disappears", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.settingsManager.setTaskModel("title", { providerId: "missing", modelId: "missing" });

		await expect(
			resolveTaskModel("title", {
				conversationModel: harness.session.model,
				taskModel: harness.settingsManager.getTaskModel("title"),
				modelRuntime: harness.session.modelRuntime,
			}),
		).rejects.toThrow("Title model missing/missing is no longer available");
	});
});
