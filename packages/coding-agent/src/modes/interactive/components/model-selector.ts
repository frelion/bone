import { type Api, type Model, modelsAreEqual } from "@frelion/bone-ai";
import {
	Container,
	type Focusable,
	fuzzyFilter,
	getKeybindings,
	Input,
	Spacer,
	Text,
	type TUI,
} from "@frelion/bone-tui";
import type { ModelRuntime } from "../../../core/model-runtime.ts";
import { getModelSelectorSearchText } from "../model-search.ts";
import { theme } from "../theme/theme.ts";
import { DynamicBorder } from "./dynamic-border.ts";
import { keyHint } from "./keybinding-hints.ts";

interface ModelItem {
	kind: "model";
	provider: string;
	id: string;
	model: Model<Api>;
}

interface FollowConversationItem {
	kind: "follow-conversation";
}

type ModelSelectorItem = ModelItem | FollowConversationItem;

export type ModelSelectorSelection = { kind: "model"; model: Model<Api> } | { kind: "follow-conversation" };

interface ScopedModelItem {
	model: Model<Api>;
	thinkingLevel?: string;
}

type ModelScope = "all" | "scoped";

/**
 * Component that renders a model selector with search
 */
export class ModelSelectorComponent extends Container implements Focusable {
	private searchInput: Input;

	// Focusable implementation - propagate to searchInput for IME cursor positioning
	private _focused = false;
	get focused(): boolean {
		return this._focused;
	}
	set focused(value: boolean) {
		this._focused = value;
		this.searchInput.focused = value;
	}
	private listContainer: Container;
	private allModels: ModelItem[] = [];
	private scopedModelItems: ModelItem[] = [];
	private activeModels: ModelItem[] = [];
	private filteredModels: ModelSelectorItem[] = [];
	private selectedIndex: number = 0;
	private currentModel?: Model<Api>;
	private modelRuntime: ModelRuntime;
	private onSelectCallback: (selection: ModelSelectorSelection) => void;
	private onCancelCallback: () => void;
	private readonly allowFollowConversation: boolean;
	private errorMessage?: string;
	private refreshStatusMessage = "Refreshing model catalogs…";
	private refreshStatusSuccess = false;
	private tui: TUI;
	private scopedModels: ReadonlyArray<ScopedModelItem>;
	private scope: ModelScope = "all";
	private scopeText?: Text;
	private scopeHintText?: Text;
	private readonly refreshAbortController = new AbortController();
	private refreshTimeout?: ReturnType<typeof setTimeout>;
	private closed = false;

	constructor(
		tui: TUI,
		currentModel: Model<Api> | undefined,
		modelRuntime: ModelRuntime,
		scopedModels: ReadonlyArray<ScopedModelItem>,
		onSelect: (selection: ModelSelectorSelection) => void,
		onCancel: () => void,
		initialSearchInput?: string,
		options?: { allowFollowConversation?: boolean },
	) {
		super();

		this.tui = tui;
		this.currentModel = currentModel;
		this.modelRuntime = modelRuntime;
		this.scopedModels = scopedModels;
		this.scope = scopedModels.length > 0 ? "scoped" : "all";
		this.onSelectCallback = onSelect;
		this.onCancelCallback = onCancel;
		this.allowFollowConversation = options?.allowFollowConversation ?? false;

		// Add top border
		this.addChild(new DynamicBorder());
		this.addChild(new Spacer(1));

		// Add hint about model filtering
		if (scopedModels.length > 0) {
			this.scopeText = new Text(this.getScopeText(), 0, 0);
			this.addChild(this.scopeText);
			this.scopeHintText = new Text(this.getScopeHintText(), 0, 0);
			this.addChild(this.scopeHintText);
		} else {
			const hintText = "Only showing models from configured providers. Use /login to add providers.";
			this.addChild(new Text(theme.fg("warning", hintText), 0, 0));
		}
		this.addChild(new Spacer(1));

		// Create search input
		this.searchInput = new Input();
		if (initialSearchInput) {
			this.searchInput.setValue(initialSearchInput);
		}
		this.searchInput.onSubmit = () => {
			// Enter on search input selects the first filtered item
			const selected = this.filteredModels[this.selectedIndex];
			if (selected) this.handleSelect(selected);
		};
		this.addChild(this.searchInput);

		this.addChild(new Spacer(1));

		// Create list container
		this.listContainer = new Container();
		this.addChild(this.listContainer);

		this.addChild(new Spacer(1));

		// Add bottom border
		this.addChild(new DynamicBorder());

		// Render the current snapshot immediately, then refresh in the background.
		this.loadModelsFromSnapshot();
		if (initialSearchInput) this.filterModels(initialSearchInput);
		else this.updateList();
		this.tui.requestRender();
		void this.refreshModels();
	}

	private loadModelsFromSnapshot(): void {
		const models = this.modelRuntime.getAvailableSnapshot().map((model: Model<Api>) => ({
			kind: "model" as const,
			provider: model.provider,
			id: model.id,
			model,
		}));
		this.allModels = this.sortModels(models);
		this.scopedModels = this.scopedModels.map((scoped) => {
			const refreshed = this.modelRuntime.getModel(scoped.model.provider, scoped.model.id);
			return refreshed ? { ...scoped, model: refreshed } : scoped;
		});
		this.scopedModelItems = this.scopedModels.map((scoped) => ({
			kind: "model" as const,
			provider: scoped.model.provider,
			id: scoped.model.id,
			model: scoped.model,
		}));
		this.activeModels = this.scope === "scoped" ? this.scopedModelItems : this.allModels;
		this.filteredModels = this.getSelectableItems(this.activeModels);
		const currentIndex = this.filteredModels.findIndex(
			(item) => item.kind === "model" && modelsAreEqual(this.currentModel, item.model),
		);
		this.selectedIndex =
			currentIndex >= 0 ? currentIndex : Math.min(this.selectedIndex, Math.max(0, this.filteredModels.length - 1));
	}

	private async refreshModels(): Promise<void> {
		const timeoutMs = 15_000;
		let timedOut = false;
		this.refreshTimeout = setTimeout(() => {
			timedOut = true;
			this.refreshAbortController.abort();
		}, timeoutMs);
		try {
			const result = await this.modelRuntime.refresh({ signal: this.refreshAbortController.signal });
			if (this.closed) return;
			this.refreshStatusMessage = "";
			if (result.aborted && timedOut) {
				this.errorMessage = "Model refresh timed out; showing cached models.";
			} else if (result.errors.size === 1) {
				this.errorMessage = `Could not refresh ${result.errors.keys().next().value}; showing cached models.`;
			} else if (result.errors.size > 1) {
				this.errorMessage = `Could not refresh ${result.errors.size} model catalogs; showing cached models.`;
			} else {
				this.errorMessage = this.modelRuntime.getError();
				if (!this.errorMessage) {
					this.refreshStatusMessage = "Model catalogs refreshed.";
					this.refreshStatusSuccess = true;
				}
			}
			this.loadModelsFromSnapshot();
			this.filterModels(this.searchInput.getValue());
			this.tui.requestRender();
		} finally {
			if (this.refreshTimeout) clearTimeout(this.refreshTimeout);
		}
	}

	private close(): void {
		this.closed = true;
		if (this.refreshTimeout) clearTimeout(this.refreshTimeout);
		this.refreshAbortController.abort();
	}

	private sortModels(models: ModelItem[]): ModelItem[] {
		const sorted = [...models];
		// Sort: current model first, then by provider
		sorted.sort((a, b) => {
			const aIsCurrent = modelsAreEqual(this.currentModel, a.model);
			const bIsCurrent = modelsAreEqual(this.currentModel, b.model);
			if (aIsCurrent && !bIsCurrent) return -1;
			if (!aIsCurrent && bIsCurrent) return 1;
			return a.provider.localeCompare(b.provider);
		});
		return sorted;
	}

	private getScopeText(): string {
		const allText = this.scope === "all" ? theme.fg("accent", "all") : theme.fg("muted", "all");
		const scopedText = this.scope === "scoped" ? theme.fg("accent", "scoped") : theme.fg("muted", "scoped");
		return `${theme.fg("muted", "Scope: ")}${allText}${theme.fg("muted", " | ")}${scopedText}`;
	}

	private getScopeHintText(): string {
		return keyHint("tui.input.tab", "scope") + theme.fg("muted", " (all/scoped)");
	}

	private setScope(scope: ModelScope): void {
		if (this.scope === scope) return;
		this.scope = scope;
		this.activeModels = this.scope === "scoped" ? this.scopedModelItems : this.allModels;
		const currentIndex = this.getSelectableItems(this.activeModels).findIndex(
			(item) => item.kind === "model" && modelsAreEqual(this.currentModel, item.model),
		);
		this.selectedIndex = currentIndex >= 0 ? currentIndex : 0;
		this.filterModels(this.searchInput.getValue());
		if (this.scopeText) {
			this.scopeText.setText(this.getScopeText());
		}
	}

	private filterModels(query: string): void {
		const models = query
			? fuzzyFilter(this.activeModels, query, ({ id, provider, model }) =>
					getModelSelectorSearchText({ id, provider, name: model.name }),
				)
			: this.activeModels;
		const followConversation =
			this.allowFollowConversation && (!query || "follow conversation".includes(query.toLowerCase()))
				? [{ kind: "follow-conversation" as const }]
				: [];
		this.filteredModels = [...followConversation, ...models];
		this.selectedIndex = Math.min(this.selectedIndex, Math.max(0, this.filteredModels.length - 1));
		this.updateList();
	}

	private updateList(): void {
		this.listContainer.clear();

		const maxVisible = 10;
		const startIndex = Math.max(
			0,
			Math.min(this.selectedIndex - Math.floor(maxVisible / 2), this.filteredModels.length - maxVisible),
		);
		const endIndex = Math.min(startIndex + maxVisible, this.filteredModels.length);

		// Show visible slice of filtered models
		for (let i = startIndex; i < endIndex; i++) {
			const item = this.filteredModels[i];
			if (!item) continue;

			const isSelected = i === this.selectedIndex;
			if (item.kind === "follow-conversation") {
				const prefix = isSelected ? theme.fg("accent", "→ ") : "  ";
				const label = isSelected
					? theme.fg("accent", "Follow Conversation")
					: theme.fg("text", "Follow Conversation");
				this.listContainer.addChild(
					new Text(`${prefix}${label} ${theme.fg("muted", "(use the current conversation model)")}`, 0, 0),
				);
				continue;
			}
			const isCurrent = modelsAreEqual(this.currentModel, item.model);

			let line = "";
			if (isSelected) {
				const prefix = theme.fg("accent", "→ ");
				const modelText = `${item.id}`;
				const providerBadge = theme.fg("muted", `[${item.provider}]`);
				const checkmark = isCurrent ? theme.fg("success", " ✓") : "";
				line = `${prefix + theme.fg("accent", modelText)} ${providerBadge}${checkmark}`;
			} else {
				const modelText = `  ${item.id}`;
				const providerBadge = theme.fg("muted", `[${item.provider}]`);
				const checkmark = isCurrent ? theme.fg("success", " ✓") : "";
				line = `${modelText} ${providerBadge}${checkmark}`;
			}

			this.listContainer.addChild(new Text(line, 0, 0));
		}

		// Add scroll indicator if needed
		if (startIndex > 0 || endIndex < this.filteredModels.length) {
			const scrollInfo = theme.fg("muted", `  (${this.selectedIndex + 1}/${this.filteredModels.length})`);
			this.listContainer.addChild(new Text(scrollInfo, 0, 0));
		}

		// Show error message or "no results" if empty
		if (this.errorMessage) {
			// Show error in red
			const errorLines = this.errorMessage.split("\n");
			for (const line of errorLines) {
				this.listContainer.addChild(new Text(theme.fg("error", line), 0, 0));
			}
		} else if (this.filteredModels.length === 0) {
			this.listContainer.addChild(new Text(theme.fg("muted", "  No matching models"), 0, 0));
		} else {
			const selected = this.filteredModels[this.selectedIndex];
			if (selected?.kind === "model") {
				this.listContainer.addChild(new Spacer(1));
				this.listContainer.addChild(new Text(theme.fg("muted", `  Model Name: ${selected.model.name}`), 0, 0));
			}
		}
		if (this.refreshStatusMessage) {
			this.listContainer.addChild(new Spacer(1));
			this.listContainer.addChild(
				new Text(theme.fg(this.refreshStatusSuccess ? "success" : "muted", `  ${this.refreshStatusMessage}`), 0, 0),
			);
		}
	}

	handleInput(keyData: string): void {
		const kb = getKeybindings();
		if (kb.matches(keyData, "tui.input.tab")) {
			if (this.scopedModelItems.length > 0) {
				const nextScope: ModelScope = this.scope === "all" ? "scoped" : "all";
				this.setScope(nextScope);
				if (this.scopeHintText) {
					this.scopeHintText.setText(this.getScopeHintText());
				}
			}
			return;
		}
		// Up arrow - wrap to bottom when at top
		if (kb.matches(keyData, "tui.select.up")) {
			if (this.filteredModels.length === 0) return;
			this.selectedIndex = this.selectedIndex === 0 ? this.filteredModels.length - 1 : this.selectedIndex - 1;
			this.updateList();
		}
		// Down arrow - wrap to top when at bottom
		else if (kb.matches(keyData, "tui.select.down")) {
			if (this.filteredModels.length === 0) return;
			this.selectedIndex = this.selectedIndex === this.filteredModels.length - 1 ? 0 : this.selectedIndex + 1;
			this.updateList();
		}
		// Enter
		else if (kb.matches(keyData, "tui.select.confirm")) {
			const selectedModel = this.filteredModels[this.selectedIndex];
			if (selectedModel) this.handleSelect(selectedModel);
		}
		// Escape or Ctrl+C
		else if (kb.matches(keyData, "tui.select.cancel")) {
			this.close();
			this.onCancelCallback();
		}
		// Pass everything else to search input
		else {
			this.searchInput.handleInput(keyData);
			this.filterModels(this.searchInput.getValue());
		}
	}

	private getSelectableItems(models: ModelItem[]): ModelSelectorItem[] {
		return this.allowFollowConversation ? [{ kind: "follow-conversation" }, ...models] : models;
	}

	private handleSelect(item: ModelSelectorItem): void {
		this.close();
		this.onSelectCallback(item.kind === "model" ? { kind: "model", model: item.model } : item);
	}

	getSearchInput(): Input {
		return this.searchInput;
	}
}
