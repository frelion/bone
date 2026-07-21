import type { Api, Model } from "@frelion/bone-ai";
import type { ModelRuntime } from "./model-runtime.ts";

export type ModelTaskId = "conversation" | "title";
export type AuxiliaryModelTaskId = Exclude<ModelTaskId, "conversation">;

export interface TaskModelReference {
	providerId: string;
	modelId: string;
}

export interface ModelTaskDefinition {
	id: ModelTaskId;
	label: string;
	description: string;
}

export const MODEL_TASK_DEFINITIONS: readonly ModelTaskDefinition[] = [
	{
		id: "conversation",
		label: "Conversation",
		description: "Used for this conversation",
	},
	{
		id: "title",
		label: "Title generation",
		description: "Used by /name",
	},
];

export type ResolvedTaskModel = {
	model: Model<Api>;
	source: "conversation" | "task-binding";
};

export async function resolveTaskModel(
	taskId: ModelTaskId,
	options: {
		conversationModel: Model<Api> | undefined;
		taskModel: TaskModelReference | undefined;
		modelRuntime: ModelRuntime;
	},
): Promise<ResolvedTaskModel> {
	const conversationModel = options.conversationModel;
	if (!conversationModel) {
		throw new Error("No conversation model is selected");
	}

	if (taskId === "conversation" || !options.taskModel) {
		if (!(await options.modelRuntime.checkAuth(conversationModel.provider))) {
			throw new Error(`No API key for ${conversationModel.provider}/${conversationModel.id}`);
		}
		return { model: conversationModel, source: "conversation" };
	}

	const { providerId, modelId } = options.taskModel;
	const model = options.modelRuntime.getModel(providerId, modelId);
	if (!model) {
		throw new Error(`Title model ${providerId}/${modelId} is no longer available. Choose another model with /model.`);
	}
	if (!(await options.modelRuntime.checkAuth(model.provider))) {
		throw new Error(`No API key for title model ${model.provider}/${model.id}`);
	}
	return { model, source: "task-binding" };
}
