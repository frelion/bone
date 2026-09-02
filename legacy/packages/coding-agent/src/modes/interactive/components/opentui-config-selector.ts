import { basename, dirname, join, relative } from "node:path";
import type { BoxRenderable, CliRenderer, Renderable } from "@opentui/core";
import { CONFIG_DIR_NAME } from "../../../config.ts";
import type { PathMetadata, ResolvedPaths, ResolvedResource } from "../../../core/resource-types.ts";
import type { SettingsManager } from "../../../core/settings-manager.ts";
import { canonicalizePath } from "../../../utils/paths.ts";
import { OpenTUISelectorViewV2 } from "./opentui-selector-v2.ts";

type ResourceType = "skills" | "prompts" | "themes";
type ConfigWriteScope = "global" | "project";
type SettingsScope = "user" | "project";
type ProjectOverrideState = "inherit" | "load" | "unload";
export type ScopedResolvedPaths = Record<ConfigWriteScope, ResolvedPaths>;

interface ResourceItem {
	key: string;
	path: string;
	enabled: boolean;
	metadata: PathMetadata;
	resourceType: ResourceType;
	displayName: string;
}

export interface OpenTUIConfigSelectorOptions {
	resolvedPaths: ScopedResolvedPaths;
	settingsManager: SettingsManager;
	cwd: string;
	agentDir: string;
	writeScope: ConfigWriteScope;
	projectModeAvailable: boolean;
	onClose: () => void;
	onExit: () => void;
}

function flattenResources(resolved: ResolvedPaths): ResourceItem[] {
	const items: ResourceItem[] = [];
	const append = (resources: ResolvedResource[], resourceType: ResourceType) => {
		for (const resource of resources) {
			if (resource.metadata.origin !== "top-level") continue;
			const fileName = basename(resource.path);
			items.push({
				key: `${resourceType}:${canonicalizePath(resource.path)}`,
				path: resource.path,
				enabled: resource.enabled,
				metadata: resource.metadata,
				resourceType,
				displayName:
					resourceType === "skills" && fileName === "SKILL.md" ? basename(dirname(resource.path)) : fileName,
			});
		}
	};
	append(resolved.skills, "skills");
	append(resolved.prompts, "prompts");
	append(resolved.themes, "themes");
	return items.sort(
		(left, right) =>
			left.resourceType.localeCompare(right.resourceType) || left.displayName.localeCompare(right.displayName),
	);
}

/** Structured local-resource config editor for `bone config`. */
export class OpenTUIConfigSelectorV2 {
	private readonly options: OpenTUIConfigSelectorOptions;
	private readonly itemsByScope: Record<ConfigWriteScope, ResourceItem[]>;
	private readonly inheritedEnabled = new Map<string, boolean>();
	private selector: OpenTUISelectorViewV2<string> | undefined;
	private writeScope: ConfigWriteScope;

	constructor(options: OpenTUIConfigSelectorOptions) {
		this.options = options;
		this.writeScope = options.writeScope;
		this.itemsByScope = {
			global: flattenResources(options.resolvedPaths.global),
			project: flattenResources(options.resolvedPaths.project),
		};
		for (const item of this.itemsByScope.global) this.inheritedEnabled.set(item.key, item.enabled);
	}

	get root(): BoxRenderable | undefined {
		return this.selector?.root;
	}

	get focusTarget(): Renderable | undefined {
		return this.selector?.focusTarget;
	}

	build(renderer: CliRenderer): BoxRenderable {
		this.selector = new OpenTUISelectorViewV2({
			title: "Local Resources",
			subtitle: "Tab scope · Space/Enter toggle · Esc close",
			items: [],
			searchable: true,
			searchPlaceholder: "Search skills, prompts, and themes",
			onSelect: () => this.toggleSelected(),
			onCancel: this.options.onClose,
		});
		const node = this.selector.build(renderer);
		this.updateItems();
		return node;
	}

	focus(): void {
		this.selector?.focus();
	}

	handleAction(action: "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown"): boolean {
		return this.selector?.handleAction(action) ?? false;
	}

	handleCommand(command: "scope" | "exit" | "toggle"): void {
		if (command === "exit") {
			this.options.onExit();
			return;
		}
		if (command === "scope") {
			if (!this.options.projectModeAvailable) return;
			this.writeScope = this.writeScope === "global" ? "project" : "global";
			this.updateItems();
			return;
		}
		this.toggleSelected();
	}

	private updateItems(): void {
		this.selector?.setStatus(
			this.writeScope === "project"
				? `Project scope · ${CONFIG_DIR_NAME}/settings.json`
				: `Global scope · ~/${CONFIG_DIR_NAME}/agent/settings.json`,
		);
		this.selector?.setItems(
			this.itemsByScope[this.writeScope].map((item) => ({
				value: item.key,
				label: `${this.checkbox(item)} ${item.displayName}`,
				description: `${item.resourceType} · ${item.path}`,
				keywords: `${item.resourceType} ${item.path}`,
				disabled: this.writeScope === "global" && item.metadata.scope === "project",
			})),
		);
	}

	private toggleSelected(): void {
		const selected = this.selector?.selectedItem;
		if (!selected) return;
		const item = this.itemsByScope[this.writeScope].find((candidate) => candidate.key === selected.value);
		if (!item) return;
		if (this.writeScope === "global" && item.metadata.scope === "project") return;
		if (this.writeScope === "project") {
			const state = this.nextOverrideState(item);
			this.setProjectOverride(item, state);
			item.enabled = state === "inherit" ? this.getInheritedEnabled(item) : state === "load";
		} else {
			item.enabled = !item.enabled;
			this.setTopLevelEnabled(item, item.enabled);
		}
		this.updateItems();
	}

	private checkbox(item: ResourceItem): string {
		if (this.writeScope !== "project") return item.enabled ? "[x]" : "[ ]";
		const state = this.getProjectOverrideState(item);
		if (state === "load") return "[+]";
		if (state === "unload") return "[-]";
		return item.enabled ? "[x]" : "[ ]";
	}

	private setTopLevelEnabled(item: ResourceItem, enabled: boolean): void {
		const scope = item.metadata.scope === "project" ? "project" : "user";
		const settings =
			scope === "project"
				? this.options.settingsManager.getProjectSettings()
				: this.options.settingsManager.getGlobalSettings();
		const current = (settings[item.resourceType] ?? []) as string[];
		const pattern = this.resourcePattern(item, scope);
		const updated = current.filter((entry) => this.entryTarget(entry) !== pattern);
		updated.push(`${enabled ? "+" : "-"}${pattern}`);
		if (scope === "project") this.setProjectPaths(item.resourceType, updated);
		else if (item.resourceType === "skills") this.options.settingsManager.setSkillPaths(updated);
		else if (item.resourceType === "prompts") this.options.settingsManager.setPromptTemplatePaths(updated);
		else this.options.settingsManager.setThemePaths(updated);
	}

	private setProjectOverride(item: ResourceItem, state: ProjectOverrideState): void {
		const current = (this.options.settingsManager.getProjectSettings()[item.resourceType] ?? []) as string[];
		const pattern = this.isInherited(item) ? item.path : this.resourcePattern(item, "project");
		const patterns = this.overridePatterns(item);
		const updated = current.filter((entry) => !patterns.has(this.entryTarget(entry)));
		if (state !== "inherit") {
			if (this.isInherited(item) && !updated.includes(pattern)) updated.push(pattern);
			updated.push(`${state === "load" ? "+" : "-"}${pattern}`);
		}
		this.setProjectPaths(item.resourceType, updated);
	}

	private setProjectPaths(type: ResourceType, paths: string[]): void {
		if (type === "skills") this.options.settingsManager.setProjectSkillPaths(paths);
		else if (type === "prompts") this.options.settingsManager.setProjectPromptTemplatePaths(paths);
		else this.options.settingsManager.setProjectThemePaths(paths);
	}

	private nextOverrideState(item: ResourceItem): ProjectOverrideState {
		const state = this.getProjectOverrideState(item);
		const inherited = this.getInheritedEnabled(item);
		if (state === "inherit") return inherited ? "unload" : "load";
		if (state === "unload") return inherited ? "load" : "inherit";
		return inherited ? "inherit" : "unload";
	}

	private getProjectOverrideState(item: ResourceItem): ProjectOverrideState {
		const patterns = this.overridePatterns(item);
		let state: ProjectOverrideState = "inherit";
		for (const entry of (this.options.settingsManager.getProjectSettings()[item.resourceType] ?? []) as string[]) {
			if (!patterns.has(this.entryTarget(entry))) continue;
			state = entry.startsWith("!") || entry.startsWith("-") ? "unload" : "load";
		}
		return state;
	}

	private overridePatterns(item: ResourceItem): Set<string> {
		const baseDir = join(this.options.cwd, CONFIG_DIR_NAME);
		const patterns = new Set([this.resourcePattern(item, "project"), item.path, relative(baseDir, item.path)]);
		if (item.metadata.baseDir) patterns.add(relative(item.metadata.baseDir, item.path));
		return patterns;
	}

	private resourcePattern(item: ResourceItem, scope: SettingsScope): string {
		const sourceScope = item.metadata.scope === "project" ? "project" : "user";
		if (scope !== sourceScope) return item.path;
		const baseDir =
			item.metadata.baseDir ??
			(scope === "project" ? join(this.options.cwd, CONFIG_DIR_NAME) : this.options.agentDir);
		return relative(baseDir, item.path);
	}

	private isInherited(item: ResourceItem): boolean {
		return item.metadata.scope !== "project" || this.inheritedEnabled.has(item.key);
	}

	private getInheritedEnabled(item: ResourceItem): boolean {
		return this.inheritedEnabled.get(item.key) ?? (item.metadata.scope !== "project" ? item.enabled : true);
	}

	private entryTarget(entry: string): string {
		return entry.startsWith("!") || entry.startsWith("+") || entry.startsWith("-") ? entry.slice(1) : entry;
	}
}
