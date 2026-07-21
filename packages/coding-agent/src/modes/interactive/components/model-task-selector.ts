import type { Api, Model } from "@frelion/bone-ai";
import {
	Container,
	type Focusable,
	type SelectItem,
	SelectList,
	type SelectListLayoutOptions,
	Spacer,
	Text,
} from "@frelion/bone-tui";
import { MODEL_TASK_DEFINITIONS, type ModelTaskId, type TaskModelReference } from "../../../core/task-model-router.ts";
import { getSelectListTheme, theme } from "../theme/theme.ts";
import { DynamicBorder } from "./dynamic-border.ts";
import { keyHint, rawKeyHint } from "./keybinding-hints.ts";

const MODEL_TASK_LAYOUT: SelectListLayoutOptions = {
	minPrimaryColumnWidth: 20,
	maxPrimaryColumnWidth: 32,
};

export interface ModelTaskSelectorOptions {
	conversationModel: Model<Api> | undefined;
	titleModel: TaskModelReference | undefined;
	onSelect: (taskId: ModelTaskId) => void;
	onCancel: () => void;
}

function formatModel(model: Model<Api> | TaskModelReference | undefined, fallback: string): string {
	if (!model) return fallback;
	if ("modelId" in model) return `${model.modelId} · ${model.providerId}`;
	return `${model.id} · ${model.provider}`;
}

/** Selects which Bone task receives a model assignment. */
export class ModelTaskSelectorComponent extends Container implements Focusable {
	private readonly selectList: SelectList;
	private _focused = false;

	get focused(): boolean {
		return this._focused;
	}

	set focused(value: boolean) {
		this._focused = value;
	}

	constructor(options: ModelTaskSelectorOptions) {
		super();
		const tasks: SelectItem[] = MODEL_TASK_DEFINITIONS.map((task) => ({
			value: task.id,
			label: task.label,
			description:
				task.id === "conversation"
					? `${formatModel(options.conversationModel, "No model selected")} · ${task.description}`
					: `${formatModel(options.titleModel, "Follow Conversation")} · ${task.description}`,
		}));

		this.addChild(new DynamicBorder());
		this.addChild(new Spacer(1));
		this.addChild(new Text(theme.fg("accent", theme.bold("Models")), 1, 0));
		this.addChild(new Text(theme.fg("muted", "Choose which task to configure"), 1, 0));
		this.addChild(new Spacer(1));

		this.selectList = new SelectList(tasks, tasks.length, getSelectListTheme(), MODEL_TASK_LAYOUT);
		this.selectList.onSelect = (item) => options.onSelect(item.value as ModelTaskId);
		this.selectList.onCancel = options.onCancel;
		this.addChild(this.selectList);
		this.addChild(new Spacer(1));
		this.addChild(
			new Text(
				`${rawKeyHint("↑↓", "select")}  ${keyHint("tui.select.confirm", "change")}  ${keyHint("tui.select.cancel", "close")}`,
				1,
				0,
			),
		);
		this.addChild(new Spacer(1));
		this.addChild(new DynamicBorder());
	}

	getSelectList(): SelectList {
		return this.selectList;
	}

	handleInput(data: string): void {
		this.selectList.handleInput(data);
	}
}
