/**
 * TUI component for managing local Bone resources (enable/disable).
 */

import { homedir } from "node:os";
import { basename, dirname, join, relative } from "node:path";
import {
	type Component,
	Container,
	type Focusable,
	getKeybindings,
	Input,
	matchesKey,
	Spacer,
	truncateToWidth,
	visibleWidth,
} from "@frelion/bone-tui";
import { CONFIG_DIR_NAME } from "../../../config.ts";
import type { PathMetadata, ResolvedPaths, ResolvedResource } from "../../../core/package-manager.ts";
import type { SettingsManager } from "../../../core/settings-manager.ts";
import { canonicalizePath } from "../../../utils/paths.ts";
import { theme } from "../theme/theme.ts";
import { DynamicBorder } from "./dynamic-border.ts";
import { keyHint, rawKeyHint } from "./keybinding-hints.ts";

type ResourceType = "skills" | "prompts" | "themes";
type ConfigWriteScope = "global" | "project";
type SettingsScope = "user" | "project";
type ProjectOverrideState = "inherit" | "load" | "unload";
export type ScopedResolvedPaths = Record<ConfigWriteScope, ResolvedPaths>;

/**
 * Resource list contract shared by the standalone config selector and the
 * settings center. The list intentionally has no persistence behavior: its
 * SettingsManager decides whether changes remain a draft or reach disk.
 */
export interface ResourceSettingsList extends Component, Focusable {
	setWriteScope(writeScope: ConfigWriteScope): void;
}

const RESOURCE_TYPE_LABELS: Record<ResourceType, string> = {
	skills: "Skills",
	prompts: "Prompts",
	themes: "Themes",
};

interface ResourceItem {
	path: string;
	enabled: boolean;
	metadata: PathMetadata;
	resourceType: ResourceType;
	displayName: string;
	groupKey: string;
	subgroupKey: string;
}

interface ResourceSubgroup {
	type: ResourceType;
	label: string;
	items: ResourceItem[];
}

interface ResourceGroup {
	key: string;
	label: string;
	scope: "user" | "project" | "temporary";
	origin: "top-level";
	source: string;
	subgroups: ResourceSubgroup[];
}

function formatBaseDir(baseDir: string): string {
	const homeDir = homedir();
	let displayPath: string;

	if (baseDir === homeDir) {
		displayPath = "~";
	} else if (baseDir.startsWith(homeDir)) {
		// Replace home prefix with ~, normalize separators for display
		const rest = baseDir.slice(homeDir.length);
		displayPath = `~${rest.replace(/\\/g, "/")}`;
	} else {
		displayPath = baseDir.replace(/\\/g, "/");
	}

	return displayPath.endsWith("/") ? displayPath : `${displayPath}/`;
}

function getGroupLabel(metadata: PathMetadata, agentDir: string): string {
	if (metadata.source === "auto") {
		if (metadata.baseDir) {
			return metadata.scope === "user"
				? `User (${formatBaseDir(metadata.baseDir)})`
				: `Project (${formatBaseDir(metadata.baseDir)})`;
		}
		return metadata.scope === "user" ? `User (${formatBaseDir(agentDir)})` : `Project (${CONFIG_DIR_NAME}/)`;
	}
	return metadata.scope === "user" ? "User settings" : "Project settings";
}

function buildGroups(resolved: ResolvedPaths, agentDir: string): ResourceGroup[] {
	const groupMap = new Map<string, ResourceGroup>();

	const addToGroup = (resources: ResolvedResource[], resourceType: ResourceType) => {
		for (const res of resources) {
			const { path, enabled, metadata } = res;
			if (metadata.origin !== "top-level") continue;
			const groupKey = `${metadata.origin}:${metadata.scope}:${metadata.source}:${metadata.baseDir ?? ""}`;

			if (!groupMap.has(groupKey)) {
				groupMap.set(groupKey, {
					key: groupKey,
					label: getGroupLabel(metadata, agentDir),
					scope: metadata.scope,
					origin: "top-level",
					source: metadata.source,
					subgroups: [],
				});
			}

			const group = groupMap.get(groupKey)!;
			const subgroupKey = `${groupKey}:${resourceType}`;

			let subgroup = group.subgroups.find((sg) => sg.type === resourceType);
			if (!subgroup) {
				subgroup = {
					type: resourceType,
					label: RESOURCE_TYPE_LABELS[resourceType],
					items: [],
				};
				group.subgroups.push(subgroup);
			}

			const fileName = basename(path);
			const parentFolder = basename(dirname(path));
			let displayName: string;
			if (resourceType === "skills" && fileName === "SKILL.md") {
				displayName = parentFolder;
			} else {
				displayName = fileName;
			}
			subgroup.items.push({
				path,
				enabled,
				metadata,
				resourceType,
				displayName,
				groupKey,
				subgroupKey,
			});
		}
	};

	addToGroup(resolved.skills, "skills");
	addToGroup(resolved.prompts, "prompts");
	addToGroup(resolved.themes, "themes");

	// Sort local resource groups with user resources before project resources.
	const groups = Array.from(groupMap.values());
	groups.sort((a, b) => {
		if (a.scope !== b.scope) {
			return a.scope === "user" ? -1 : 1;
		}
		return a.source.localeCompare(b.source);
	});

	// Sort subgroups within each group by type order, and items by name
	const typeOrder: Record<ResourceType, number> = { skills: 0, prompts: 1, themes: 2 };
	for (const group of groups) {
		group.subgroups.sort((a, b) => typeOrder[a.type] - typeOrder[b.type]);
		for (const subgroup of group.subgroups) {
			subgroup.items.sort((a, b) => a.displayName.localeCompare(b.displayName));
		}
	}

	return groups;
}

type FlatEntry =
	| { type: "group"; group: ResourceGroup }
	| { type: "subgroup"; subgroup: ResourceSubgroup; group: ResourceGroup }
	| { type: "item"; item: ResourceItem };

class ConfigSelectorHeader implements Component {
	private writeScope: ConfigWriteScope;
	private projectModeAvailable: boolean;
	private transactional: boolean;

	constructor(writeScope: ConfigWriteScope, projectModeAvailable: boolean, transactional = false) {
		this.writeScope = writeScope;
		this.projectModeAvailable = projectModeAvailable;
		this.transactional = transactional;
	}

	setWriteScope(writeScope: ConfigWriteScope): void {
		this.writeScope = writeScope;
	}

	invalidate(): void {}

	render(width: number): string[] {
		const title = theme.bold(this.writeScope === "project" ? "Project Local Resources" : "Global Resources");
		const sep = theme.fg("muted", " · ");
		const switchHint = this.projectModeAvailable ? keyHint("tui.input.tab", "switch mode") + sep : "";
		const actionHint =
			this.writeScope === "project" ? rawKeyHint("space", "cycle inherit/+/-") : rawKeyHint("space", "toggle");
		const saveHint = this.transactional ? rawKeyHint("ctrl+s", "save") + sep : "";
		const hint =
			switchHint + actionHint + sep + saveHint + rawKeyHint("esc", this.transactional ? "cancel" : "close");
		const spacing = Math.max(1, width - visibleWidth(title) - visibleWidth(hint));
		const scopeHint =
			this.writeScope === "project"
				? theme.fg("muted", `${CONFIG_DIR_NAME}/settings.json · inherited global resources are dimmed`)
				: theme.fg("muted", `~/${CONFIG_DIR_NAME}/agent/settings.json`);

		return [
			truncateToWidth(`${title}${" ".repeat(spacing)}${hint}`, width, ""),
			truncateToWidth(scopeHint, width, ""),
		];
	}
}

class ResourceList implements ResourceSettingsList {
	private groupsByScope: Record<ConfigWriteScope, ResourceGroup[]>;
	private flatItems: FlatEntry[] = [];
	private filteredItems: FlatEntry[] = [];
	private selectedIndex = 0;
	private searchInput: Input;
	private maxVisible: number;
	private settingsManager: SettingsManager;
	private cwd: string;
	private agentDir: string;
	private writeScope: ConfigWriteScope;
	private inheritedEnabledByKey: Map<string, boolean>;

	public onCancel?: () => void;
	public onSave?: () => void;
	public onExit?: () => void;
	public onToggle?: (item: ResourceItem, newEnabled: boolean) => void;
	public onSwitchMode?: () => void;

	private _focused = false;
	get focused(): boolean {
		return this._focused;
	}
	set focused(value: boolean) {
		this._focused = value;
		this.searchInput.focused = value;
	}

	constructor(
		groupsByScope: Record<ConfigWriteScope, ResourceGroup[]>,
		settingsManager: SettingsManager,
		cwd: string,
		agentDir: string,
		terminalHeight?: number,
		writeScope: ConfigWriteScope = "global",
	) {
		this.groupsByScope = groupsByScope;
		this.settingsManager = settingsManager;
		this.cwd = cwd;
		this.agentDir = agentDir;
		this.writeScope = writeScope;
		this.inheritedEnabledByKey = this.buildInheritedEnabledMap(groupsByScope.global);
		this.searchInput = new Input();
		// 8 lines of chrome: top spacer + top border + spacer + header (2 lines) + spacer + bottom spacer + bottom border
		const chrome = 8;
		this.maxVisible = Math.max(5, (terminalHeight ?? 24) - chrome);
		this.buildFlatList();
		this.filteredItems = [...this.flatItems];
	}

	setWriteScope(writeScope: ConfigWriteScope): void {
		this.writeScope = writeScope;
		this.buildFlatList();
		this.filterItems(this.searchInput.getValue());
	}

	private get groups(): ResourceGroup[] {
		return this.groupsByScope[this.writeScope];
	}

	private buildInheritedEnabledMap(groups: ResourceGroup[]): Map<string, boolean> {
		const result = new Map<string, boolean>();
		for (const group of groups) {
			for (const subgroup of group.subgroups) {
				for (const item of subgroup.items) {
					result.set(this.getResourceItemKey(item), item.enabled);
				}
			}
		}
		return result;
	}

	private buildFlatList(): void {
		this.flatItems = [];
		for (const group of this.groups) {
			this.flatItems.push({ type: "group", group });
			for (const subgroup of group.subgroups) {
				this.flatItems.push({ type: "subgroup", subgroup, group });
				for (const item of subgroup.items) {
					this.flatItems.push({ type: "item", item });
				}
			}
		}
		// Start selection on first item (not header)
		this.selectedIndex = this.flatItems.findIndex((e) => e.type === "item");
		if (this.selectedIndex < 0) this.selectedIndex = 0;
	}

	private findNextItem(fromIndex: number, direction: 1 | -1): number {
		let idx = fromIndex + direction;
		while (idx >= 0 && idx < this.filteredItems.length) {
			if (this.filteredItems[idx].type === "item") {
				return idx;
			}
			idx += direction;
		}
		return fromIndex; // Stay at current if no item found
	}

	private filterItems(query: string): void {
		if (!query.trim()) {
			this.filteredItems = [...this.flatItems];
			this.selectFirstItem();
			return;
		}

		const lowerQuery = query.toLowerCase();
		const matchingItems = new Set<ResourceItem>();
		const matchingSubgroups = new Set<ResourceSubgroup>();
		const matchingGroups = new Set<ResourceGroup>();

		for (const entry of this.flatItems) {
			if (entry.type === "item") {
				const item = entry.item;
				if (
					item.displayName.toLowerCase().includes(lowerQuery) ||
					item.resourceType.toLowerCase().includes(lowerQuery) ||
					item.path.toLowerCase().includes(lowerQuery)
				) {
					matchingItems.add(item);
				}
			}
		}

		// Find which subgroups and groups contain matching items
		for (const group of this.groups) {
			for (const subgroup of group.subgroups) {
				for (const item of subgroup.items) {
					if (matchingItems.has(item)) {
						matchingSubgroups.add(subgroup);
						matchingGroups.add(group);
					}
				}
			}
		}

		this.filteredItems = [];
		for (const entry of this.flatItems) {
			if (entry.type === "group" && matchingGroups.has(entry.group)) {
				this.filteredItems.push(entry);
			} else if (entry.type === "subgroup" && matchingSubgroups.has(entry.subgroup)) {
				this.filteredItems.push(entry);
			} else if (entry.type === "item" && matchingItems.has(entry.item)) {
				this.filteredItems.push(entry);
			}
		}

		this.selectFirstItem();
	}

	private selectFirstItem(): void {
		const firstItemIndex = this.filteredItems.findIndex((e) => e.type === "item");
		this.selectedIndex = firstItemIndex >= 0 ? firstItemIndex : 0;
	}

	updateItem(item: ResourceItem, enabled: boolean): void {
		item.enabled = enabled;
		// Update in groups too
		for (const group of this.groups) {
			for (const subgroup of group.subgroups) {
				const found = subgroup.items.find((i) => i.path === item.path && i.resourceType === item.resourceType);
				if (found) {
					found.enabled = enabled;
					return;
				}
			}
		}
	}

	invalidate(): void {}

	render(width: number): string[] {
		const lines: string[] = [];

		// Search input
		lines.push(...this.searchInput.render(width));
		lines.push("");

		if (this.filteredItems.length === 0) {
			lines.push(theme.fg("muted", "  No resources found"));
			return lines;
		}

		// Calculate visible range
		const startIndex = Math.max(
			0,
			Math.min(this.selectedIndex - Math.floor(this.maxVisible / 2), this.filteredItems.length - this.maxVisible),
		);
		const endIndex = Math.min(startIndex + this.maxVisible, this.filteredItems.length);

		for (let i = startIndex; i < endIndex; i++) {
			const entry = this.filteredItems[i];
			const isSelected = i === this.selectedIndex;

			if (entry.type === "group") {
				// Main group header (no cursor)
				const inherited = this.writeScope === "project" && entry.group.scope === "user";
				const label = theme.bold(`${entry.group.label}${inherited ? " · inherited global" : ""}`);
				const groupLine = theme.fg(inherited ? "dim" : "accent", label);
				lines.push(truncateToWidth(`  ${groupLine}`, width, ""));
			} else if (entry.type === "subgroup") {
				// Subgroup header (indented, no cursor)
				const color = this.writeScope === "project" && entry.group.scope === "user" ? "dim" : "muted";
				const subgroupLine = theme.fg(color, entry.subgroup.label);
				lines.push(truncateToWidth(`    ${subgroupLine}`, width, ""));
			} else {
				// Resource item (cursor only on items)
				const item = entry.item;
				const cursor = isSelected ? "> " : "  ";
				const dimmed = this.isDimmedItem(item);
				const nameText = isSelected && !dimmed ? theme.bold(item.displayName) : item.displayName;
				const name = dimmed ? theme.fg("dim", nameText) : nameText;
				lines.push(
					truncateToWidth(
						`${cursor}    ${this.renderCheckbox(item)} ${name}${this.getItemSuffix(item)}`,
						width,
						"...",
					),
				);
			}
		}

		// Scroll indicator
		if (startIndex > 0 || endIndex < this.filteredItems.length) {
			const itemCount = this.filteredItems.filter((e) => e.type === "item").length;
			const currentItemIndex =
				this.filteredItems.slice(0, this.selectedIndex).filter((e) => e.type === "item").length + 1;
			lines.push(theme.fg("dim", `  (${currentItemIndex}/${itemCount})`));
		}

		return lines;
	}

	handleInput(data: string): void {
		const kb = getKeybindings();

		if (kb.matches(data, "tui.select.up")) {
			this.selectedIndex = this.findNextItem(this.selectedIndex, -1);
			return;
		}
		if (kb.matches(data, "tui.select.down")) {
			this.selectedIndex = this.findNextItem(this.selectedIndex, 1);
			return;
		}
		if (kb.matches(data, "tui.select.pageUp")) {
			// Jump up by maxVisible, then find nearest item
			let target = Math.max(0, this.selectedIndex - this.maxVisible);
			while (target < this.filteredItems.length && this.filteredItems[target].type !== "item") {
				target++;
			}
			if (target < this.filteredItems.length) {
				this.selectedIndex = target;
			}
			return;
		}
		if (kb.matches(data, "tui.select.pageDown")) {
			// Jump down by maxVisible, then find nearest item
			let target = Math.min(this.filteredItems.length - 1, this.selectedIndex + this.maxVisible);
			while (target >= 0 && this.filteredItems[target].type !== "item") {
				target--;
			}
			if (target >= 0) {
				this.selectedIndex = target;
			}
			return;
		}
		if (kb.matches(data, "tui.select.cancel")) {
			this.onCancel?.();
			return;
		}
		if (kb.matches(data, "app.settings.save")) {
			this.onSave?.();
			return;
		}
		if (matchesKey(data, "ctrl+c")) {
			this.onExit?.();
			return;
		}
		if (kb.matches(data, "tui.input.tab")) {
			this.onSwitchMode?.();
			return;
		}
		if (data === " " || kb.matches(data, "tui.select.confirm")) {
			const entry = this.filteredItems[this.selectedIndex];
			if (entry?.type === "item" && (this.writeScope === "project" || this.getItemScope(entry.item) === "user")) {
				const newEnabled = this.toggleResource(entry.item);
				if (newEnabled !== undefined) {
					this.updateItem(entry.item, newEnabled);
					this.onToggle?.(entry.item, newEnabled);
				}
			}
			return;
		}

		// Pass to search input
		this.searchInput.handleInput(data);
		this.filterItems(this.searchInput.getValue());
	}

	private toggleResource(item: ResourceItem): boolean | undefined {
		if (this.writeScope === "project") {
			const state = this.getNextOverrideState(item);
			if (!this.setProjectResourceOverride(item, state)) return undefined;
			return state === "inherit" ? this.getInheritedEnabled(item) : state === "load";
		}

		const enabled = !item.enabled;
		this.toggleTopLevelResource(item, enabled);
		return enabled;
	}

	private toggleTopLevelResource(item: ResourceItem, enabled: boolean): void {
		const scope = item.metadata.scope as "user" | "project";
		const settings =
			scope === "project" ? this.settingsManager.getProjectSettings() : this.settingsManager.getGlobalSettings();

		const arrayKey = item.resourceType;
		const current = (settings[arrayKey] ?? []) as string[];

		// Generate pattern for this resource
		const pattern = this.getResourcePattern(item);
		const disablePattern = `-${pattern}`;
		const enablePattern = `+${pattern}`;

		// Filter out existing patterns for this resource
		const updated = current.filter((p) => {
			const stripped = p.startsWith("!") || p.startsWith("+") || p.startsWith("-") ? p.slice(1) : p;
			return stripped !== pattern;
		});

		if (enabled) {
			updated.push(enablePattern);
		} else {
			updated.push(disablePattern);
		}

		if (scope === "project") {
			if (arrayKey === "skills") {
				this.settingsManager.setProjectSkillPaths(updated);
			} else if (arrayKey === "prompts") {
				this.settingsManager.setProjectPromptTemplatePaths(updated);
			} else if (arrayKey === "themes") {
				this.settingsManager.setProjectThemePaths(updated);
			}
		} else {
			if (arrayKey === "skills") {
				this.settingsManager.setSkillPaths(updated);
			} else if (arrayKey === "prompts") {
				this.settingsManager.setPromptTemplatePaths(updated);
			} else if (arrayKey === "themes") {
				this.settingsManager.setThemePaths(updated);
			}
		}
	}

	private renderCheckbox(item: ResourceItem): string {
		if (this.writeScope === "project") {
			const state = this.getProjectOverrideState(item);
			if (state === "load") return theme.fg("success", "[+]");
			if (state === "unload") return theme.fg("warning", "[-]");
			return theme.fg("dim", item.enabled ? "[x]" : "[ ]");
		}
		return item.enabled ? theme.fg("success", "[x]") : theme.fg("dim", "[ ]");
	}

	private getItemSuffix(item: ResourceItem): string {
		if (this.writeScope !== "project") return "";
		const state = this.getProjectOverrideState(item);
		if (state === "load") return theme.fg("muted", "  project load");
		if (state === "unload") return theme.fg("muted", "  project unload");
		return this.isInheritedGlobalItem(item) ? theme.fg("dim", "  inherited global") : "";
	}

	private isDimmedItem(item: ResourceItem): boolean {
		return (
			this.writeScope === "project" &&
			this.isInheritedGlobalItem(item) &&
			this.getProjectOverrideState(item) === "inherit"
		);
	}

	private setProjectResourceOverride(item: ResourceItem, state: ProjectOverrideState): boolean {
		return this.setProjectTopLevelOverride(item, state);
	}

	private setProjectTopLevelOverride(item: ResourceItem, state: ProjectOverrideState): boolean {
		const current = (this.settingsManager.getProjectSettings()[item.resourceType] ?? []) as string[];
		const pattern = this.isInheritedGlobalItem(item) ? item.path : this.getResourcePatternForScope(item, "project");
		const patterns = this.getTopLevelOverridePatterns(item, "project");
		const updated = current.filter((entry) => {
			const target = this.getPatternEntryTarget(entry);
			if ((entry.startsWith("!") || entry.startsWith("+") || entry.startsWith("-")) && patterns.has(target))
				return false;
			return !(state === "inherit" && this.isInheritedGlobalItem(item) && target === pattern);
		});
		if (state !== "inherit") {
			if (this.isInheritedGlobalItem(item) && !updated.includes(pattern)) updated.push(pattern);
			updated.push(`${state === "load" ? "+" : "-"}${pattern}`);
		}
		this.setProjectTopLevelPaths(item.resourceType, updated);
		return true;
	}

	private setProjectTopLevelPaths(key: ResourceType, paths: string[]): void {
		if (key === "skills") this.settingsManager.setProjectSkillPaths(paths);
		else if (key === "prompts") this.settingsManager.setProjectPromptTemplatePaths(paths);
		else this.settingsManager.setProjectThemePaths(paths);
	}

	private getNextOverrideState(item: ResourceItem): ProjectOverrideState {
		const state = this.getProjectOverrideState(item);
		const inheritedEnabled = this.getInheritedEnabled(item);
		if (state === "inherit") return inheritedEnabled ? "unload" : "load";
		if (state === "unload") return inheritedEnabled ? "load" : "inherit";
		return inheritedEnabled ? "inherit" : "unload";
	}

	private getProjectOverrideState(item: ResourceItem): ProjectOverrideState {
		if (this.writeScope !== "project") return "inherit";
		return this.getOverrideStateFromEntries(
			(this.settingsManager.getProjectSettings()[item.resourceType] ?? []) as string[],
			this.getTopLevelOverridePatterns(item, "project"),
			false,
		);
	}

	private getOverrideStateFromEntries(
		entries: string[],
		patterns: Set<string>,
		emptyArrayIsUnload: boolean,
	): ProjectOverrideState {
		if (entries.length === 0 && emptyArrayIsUnload) return "unload";
		let state: ProjectOverrideState = "inherit";
		for (const entry of entries) {
			if (!patterns.has(this.getPatternEntryTarget(entry))) continue;
			if (entry.startsWith("!") || entry.startsWith("-")) state = "unload";
			else state = "load";
		}
		return state;
	}

	private getInheritedEnabled(item: ResourceItem): boolean {
		return (
			this.inheritedEnabledByKey.get(this.getResourceItemKey(item)) ??
			(this.getItemScope(item) === "user" ? item.enabled : true)
		);
	}

	private isInheritedGlobalItem(item: ResourceItem): boolean {
		return this.getItemScope(item) === "user" || this.inheritedEnabledByKey.has(this.getResourceItemKey(item));
	}

	private getTopLevelOverridePatterns(item: ResourceItem, scope: SettingsScope): Set<string> {
		const baseDir = this.getTopLevelBaseDir(scope);
		const patterns = new Set<string>([
			this.getResourcePatternForScope(item, scope),
			item.path,
			relative(baseDir, item.path),
		]);
		if (item.metadata.baseDir) patterns.add(relative(item.metadata.baseDir, item.path));
		return patterns;
	}

	private getResourcePatternForScope(item: ResourceItem, scope: SettingsScope): string {
		const sourceScope = this.getItemScope(item);
		if (scope !== sourceScope) return item.path;
		const baseDir = item.metadata.baseDir ?? this.getTopLevelBaseDir(sourceScope);
		return relative(baseDir, item.path);
	}

	private getPatternEntryTarget(entry: string): string {
		return entry.startsWith("!") || entry.startsWith("+") || entry.startsWith("-") ? entry.slice(1) : entry;
	}

	private getResourceItemKey(item: ResourceItem): string {
		return `${item.resourceType}:${canonicalizePath(item.path)}`;
	}

	private getItemScope(item: ResourceItem): SettingsScope {
		return item.metadata.scope === "project" ? "project" : "user";
	}

	private getTopLevelBaseDir(scope: "user" | "project"): string {
		return scope === "project" ? join(this.cwd, CONFIG_DIR_NAME) : this.agentDir;
	}

	private getResourcePattern(item: ResourceItem): string {
		const scope = item.metadata.scope as "user" | "project";
		const baseDir = item.metadata.baseDir ?? this.getTopLevelBaseDir(scope);
		return relative(baseDir, item.path);
	}
}

/** Create the unframed resource tree for embedding in another TUI surface. */
export function createResourceSettingsList(
	resolvedPaths: ScopedResolvedPaths,
	settingsManager: SettingsManager,
	cwd: string,
	agentDir: string,
	terminalHeight?: number,
	writeScope: ConfigWriteScope = "global",
): ResourceSettingsList {
	const groupsByScope = {
		global: buildGroups(resolvedPaths.global, agentDir),
		project: buildGroups(resolvedPaths.project, agentDir),
	};
	return new ResourceList(groupsByScope, settingsManager, cwd, agentDir, terminalHeight, writeScope);
}

export class ConfigSelectorComponent extends Container implements Focusable {
	private header: ConfigSelectorHeader;
	private resourceList: ResourceList;
	private writeScope: ConfigWriteScope;

	private _focused = false;
	get focused(): boolean {
		return this._focused;
	}
	set focused(value: boolean) {
		this._focused = value;
		this.resourceList.focused = value;
	}

	constructor(
		resolvedPaths: ScopedResolvedPaths,
		settingsManager: SettingsManager,
		cwd: string,
		agentDir: string,
		onClose: () => void,
		onExit: () => void,
		requestRender: () => void,
		terminalHeight?: number,
		writeScope: ConfigWriteScope = "global",
		projectModeAvailable = true,
		onSave?: () => void,
		transactional = false,
	) {
		super();

		this.writeScope = writeScope;
		// Add header
		this.addChild(new Spacer(1));
		this.addChild(new DynamicBorder());
		this.addChild(new Spacer(1));
		this.header = new ConfigSelectorHeader(this.writeScope, projectModeAvailable, transactional);
		this.addChild(this.header);
		this.addChild(new Spacer(1));

		// Resource list
		this.resourceList = createResourceSettingsList(
			resolvedPaths,
			settingsManager,
			cwd,
			agentDir,
			terminalHeight,
			this.writeScope,
		) as ResourceList;
		this.resourceList.onCancel = onClose;
		this.resourceList.onSave = onSave;
		this.resourceList.onExit = onExit;
		this.resourceList.onToggle = () => requestRender();
		if (projectModeAvailable) {
			this.resourceList.onSwitchMode = () => {
				this.switchWriteScope();
				requestRender();
			};
		}
		this.addChild(this.resourceList);

		// Bottom border
		this.addChild(new Spacer(1));
		this.addChild(new DynamicBorder());
	}

	private switchWriteScope(): void {
		this.writeScope = this.writeScope === "global" ? "project" : "global";
		this.header.setWriteScope(this.writeScope);
		this.resourceList.setWriteScope(this.writeScope);
	}

	getResourceList(): ResourceList {
		return this.resourceList;
	}
}
