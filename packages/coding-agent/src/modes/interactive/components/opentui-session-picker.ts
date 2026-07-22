import type { BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import { getAgentDir } from "../../../config.ts";
import type { SessionInfo, SessionListProgress } from "../../../core/session-manager.ts";
import { softDeleteSessionFile } from "../../../core/session-trash.ts";
import { OpenTUISelectorViewV2 } from "./opentui-selector-v2.ts";
import { filterAndSortSessions, hasSessionName, type SortMode } from "./session-selector-search.ts";

type SessionsLoader = (onProgress?: SessionListProgress) => Promise<SessionInfo[]>;
type SessionScope = "current" | "all";

function formatAge(date: Date): string {
	const minutes = Math.floor((Date.now() - date.getTime()) / 60_000);
	if (minutes < 1) return "now";
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h`;
	const days = Math.floor(hours / 24);
	if (days < 30) return `${days}d`;
	return `${Math.floor(days / 30)}mo`;
}

export interface OpenTUISessionPickerOptions {
	currentSessionsLoader: SessionsLoader;
	allSessionsLoader: SessionsLoader;
	onSelect: (path: string) => void;
	onCancel: () => void;
	onExit: () => void;
}

/** Structured startup conversation picker with fixed product commands. */
export class OpenTUISessionPickerV2 implements BoneView {
	private readonly options: OpenTUISessionPickerOptions;
	private selector: OpenTUISelectorViewV2<string> | undefined;
	private scope: SessionScope = "current";
	private sortMode: SortMode = "threaded";
	private namedOnly = false;
	private showPath = false;
	private currentSessions: SessionInfo[] | undefined;
	private allSessions: SessionInfo[] | undefined;
	private confirmingDelete: string | undefined;
	private loadSequence = 0;

	constructor(options: OpenTUISessionPickerOptions) {
		this.options = options;
	}

	mount(context: BoneRenderContext): BoneNode {
		this.selector = new OpenTUISelectorViewV2({
			title: "Conversations",
			subtitle: "Tab scope · Ctrl+S sort · Ctrl+N named · Ctrl+P path · Ctrl+D delete",
			items: [],
			searchable: true,
			searchPlaceholder: "Search conversations",
			onSelect: this.options.onSelect,
			onCancel: this.options.onCancel,
		});
		const node = this.selector.mount(context);
		void this.loadScope("current");
		return node;
	}

	handleAction(action: "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown"): boolean {
		if (this.confirmingDelete) {
			if (action === "confirm") void this.deleteConfirmed();
			else if (action === "cancel") {
				this.confirmingDelete = undefined;
				this.selector?.setStatus(undefined);
			}
			return true;
		}
		return this.selector?.handleAction(action) ?? false;
	}

	handleCommand(command: "scope" | "exit" | "sort" | "named" | "path" | "delete"): void {
		if (command === "exit") {
			this.options.onExit();
			return;
		}
		if (command === "scope") {
			this.scope = this.scope === "current" ? "all" : "current";
			const sessions = this.scope === "current" ? this.currentSessions : this.allSessions;
			if (sessions) this.updateItems();
			else void this.loadScope(this.scope);
			return;
		}
		if (command === "sort") {
			this.sortMode =
				this.sortMode === "threaded" ? "recent" : this.sortMode === "recent" ? "relevance" : "threaded";
			this.updateItems();
			return;
		}
		if (command === "named") {
			this.namedOnly = !this.namedOnly;
			this.updateItems();
			return;
		}
		if (command === "path") {
			this.showPath = !this.showPath;
			this.updateItems();
			return;
		}
		const selected = this.selector?.selectedItem;
		if (!selected) return;
		this.confirmingDelete = selected.value;
		this.selector?.setStatus("Delete conversation? Enter confirm · Esc cancel");
	}

	private async loadScope(scope: SessionScope): Promise<void> {
		const sequence = ++this.loadSequence;
		this.selector?.setStatus("Loading conversations...");
		try {
			const loader = scope === "current" ? this.options.currentSessionsLoader : this.options.allSessionsLoader;
			const sessions = await loader((loaded, total) => {
				if (sequence === this.loadSequence) this.selector?.setStatus(`Loading conversations ${loaded}/${total}`);
			});
			if (scope === "current") this.currentSessions = sessions;
			else this.allSessions = sessions;
			if (sequence !== this.loadSequence || scope !== this.scope) return;
			this.selector?.setStatus(undefined);
			this.updateItems();
		} catch (error) {
			if (sequence !== this.loadSequence || scope !== this.scope) return;
			this.selector?.setStatus(
				`Failed to load conversations: ${error instanceof Error ? error.message : String(error)}`,
			);
			this.selector?.setItems([]);
		}
	}

	private updateItems(): void {
		const source = this.scope === "current" ? (this.currentSessions ?? []) : (this.allSessions ?? []);
		const sessions = this.namedOnly ? source.filter(hasSessionName) : source;
		const sorted = filterAndSortSessions(sessions, "", this.sortMode, "all");
		this.selector?.setItems(
			sorted.map((session) => ({
				value: session.path,
				label:
					(session.name ?? session.firstMessage).replace(/[\x00-\x1f\x7f]/g, " ").trim() || "(empty conversation)",
				description: [
					this.scope === "all" && session.cwd ? session.cwd : undefined,
					this.showPath ? session.path : undefined,
					`${session.messageCount} messages`,
					formatAge(session.modified),
				]
					.filter((value): value is string => value !== undefined)
					.join(" · "),
				keywords: `${session.allMessagesText} ${session.cwd} ${session.path}`,
			})),
		);
		this.selector?.setStatus(
			`${this.scope === "current" ? "Current folder" : "All folders"} · ${this.namedOnly ? "Named" : "All"} · Sort: ${this.sortMode}`,
		);
	}

	private async deleteConfirmed(): Promise<void> {
		const path = this.confirmingDelete;
		this.confirmingDelete = undefined;
		if (!path) return;
		const result = await softDeleteSessionFile(path, getAgentDir());
		if (!result.ok) {
			this.selector?.setStatus(`Failed to delete: ${result.error ?? "Unknown error"}`);
			return;
		}
		if (this.currentSessions) this.currentSessions = this.currentSessions.filter((session) => session.path !== path);
		if (this.allSessions) this.allSessions = this.allSessions.filter((session) => session.path !== path);
		this.selector?.setStatus("Conversation moved to Trash");
		this.updateItems();
	}
}
