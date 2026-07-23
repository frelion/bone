import { basename, resolve } from "node:path";
import type { ImageContent } from "@frelion/bone-ai/compat";
import { type AutocompleteProvider, createRenderer, OverlayManager } from "@frelion/bone-tui";
import type { CliRenderer, KeyEvent } from "@opentui/core";
import type { AgentSessionEvent, PromptOptions } from "../../core/agent-session.ts";
import type { AgentSessionRuntime } from "../../core/agent-session-runtime.ts";
import { rememberLastActiveConversation } from "../../core/conversation-state.ts";
import type { ExtensionUIV2Context } from "../../core/extensions/ui-v2.ts";
import { FooterDataProvider } from "../../core/footer-data-provider.ts";
import type {
	InteractiveSessionHostHooks,
	InteractiveSessionSummary,
	RuntimeEventEnvelope,
	RuntimeStreamSnapshot,
} from "../../core/interactive-session-host.ts";
import type { LocalEmbeddingStatus } from "../../core/local-embedding.ts";
import { MemoryRuntime } from "../../core/memory.ts";
import type { PlanProposal } from "../../core/plan-mode.ts";
import type { QuestionAnswer, QuestionRequest } from "../../core/question.ts";
import { hasTrustRequiringProjectResources, ProjectTrustStore } from "../../core/trust-manager.ts";
import { OpenTUITopBar, OpenTUIWelcome } from "./components/opentui-chrome.ts";
import { OpenTUIComposer, type OpenTUIComposerStatus } from "./components/opentui-composer.ts";
import { OpenTUIStatusView } from "./components/opentui-rich-messages.ts";
import { OpenTUISessionSidebar } from "./components/opentui-session-sidebar.ts";
import { OpenTUITranscriptFactory } from "./components/opentui-transcript-factory.ts";
import { OpenTUITranscriptFocusController } from "./components/opentui-transcript-focus.ts";
import { OpenTUIPaneNavigator } from "./components/pane-navigator.ts";
import { OpenTUICommandRouter } from "./opentui-command-router.ts";
import { OpenTUIExtensionHost } from "./opentui-extension-host.ts";
import { matchesOpenTUIAction } from "./opentui-keymap.ts";
import { OpenTUIInteractiveShell } from "./opentui-shell.ts";
import { theme } from "./theme/theme.ts";

const SIDEBAR_PAGE_SIZE = 40;
const LEXICAL_SEARCH_DELAY_MS = 80;
const SEMANTIC_SEARCH_DELAY_MS = 250;
const CUSTOM_ANSWER = "custom";
const SUBMIT_SELECTION = "submit";

type OpenTUIMemoryRuntime = Pick<
	MemoryRuntime,
	"start" | "recordPersistedEntries" | "recordCompletedRun" | "removeSession" | "search" | "searchSemantic" | "dispose"
>;

interface OpenTUIMemoryRuntimeOptions {
	agentDir: string;
	cwd: string;
	onStatus: (status: ReturnType<MemoryRuntime["getStatus"]>) => void;
	onEmbeddingStatus: (status: LocalEmbeddingStatus | undefined) => void;
	onSearchRefresh: () => void;
}

interface ConversationViewState {
	draft: string;
	scrollTop: number;
}

export interface OpenTUISessionHostContract {
	readonly current: AgentSessionRuntime;
	setHooks(hooks: InteractiveSessionHostHooks): void;
	prompt(runtime: AgentSessionRuntime, text: string, options?: PromptOptions): Promise<void>;
	activate(sessionPath: string): Promise<void>;
	createNew(): Promise<void>;
	deleteSession(sessionPath: string, replacementSessionPath?: string): Promise<unknown>;
	listPage(
		offset: number,
		limit: number,
	): Promise<{ sessions: InteractiveSessionSummary[]; total: number; hasMore: boolean; nextOffset: number }>;
	getSessionState(sessionPath: string): InteractiveSessionSummary["state"];
	getSessionPresentation(
		sessionPath: string,
	): Pick<
		InteractiveSessionSummary,
		"state" | "livePreview" | "throughputTokensPerSecond" | "messageCount" | "modified"
	>;
	getSessionSummaries(paths: readonly string[]): Promise<InteractiveSessionSummary[]>;
	getRuntimeStreamSnapshot?(runtime: AgentSessionRuntime): RuntimeStreamSnapshot;
	subscribeRuntime?(runtime: AgentSessionRuntime, listener: (envelope: RuntimeEventEnvelope) => void): () => void;
	refreshForeground?(task: (runtime: AgentSessionRuntime) => Promise<void>): Promise<void>;
	disposeAll(): Promise<void>;
}

export interface OpenTUIExtensionBinding {
	readonly context: ExtensionUIV2Context;
	getToolRenderer?: OpenTUIExtensionHost["getToolRenderer"];
	dispose(): void;
}

export interface OpenTUIInteractiveModeOptions {
	migratedProviders?: string[];
	modelFallbackMessage?: string;
	autoTrustOnReloadCwd?: string;
	initialMessage?: string;
	initialImages?: ImageContent[];
	initialMessages?: string[];
	autocompleteProvider?: AutocompleteProvider;
	createRenderer?: () => Promise<CliRenderer>;
	createTranscriptFactory?: (renderer: CliRenderer) => OpenTUITranscriptFactory;
	bindExtensionUI?: (runtime: AgentSessionRuntime, renderer: CliRenderer) => OpenTUIExtensionBinding;
	createMemoryRuntime?: (options: OpenTUIMemoryRuntimeOptions) => OpenTUIMemoryRuntime;
	installSignalHandlers?: boolean;
	verbose?: boolean;
}

function messageText(message: { content?: unknown }): string {
	if (!("content" in message)) return "";
	if (typeof message.content === "string") return message.content;
	if (!Array.isArray(message.content)) return "";
	return message.content
		.filter(
			(part): part is { type: "text"; text: string } =>
				typeof part === "object" &&
				part !== null &&
				"type" in part &&
				part.type === "text" &&
				"text" in part &&
				typeof part.text === "string",
		)
		.map((part) => part.text)
		.join("\n");
}

/** Production OpenTUI owner for one foreground interactive session. */
export class OpenTUIInteractiveMode {
	private readonly sessionHost: OpenTUISessionHostContract;
	private readonly options: OpenTUIInteractiveModeOptions;
	private transcriptFactory!: OpenTUITranscriptFactory;
	private readonly commandRouter: OpenTUICommandRouter;
	private readonly memory: OpenTUIMemoryRuntime;
	private renderer: CliRenderer | undefined;
	private shell: OpenTUIInteractiveShell | undefined;
	private composer: OpenTUIComposer | undefined;
	private topBar: OpenTUITopBar | undefined;
	private welcome: OpenTUIWelcome | undefined;
	private sidebar: OpenTUISessionSidebar | undefined;
	private paneFocus: OpenTUIPaneNavigator | undefined;
	private overlayManager: OverlayManager | undefined;
	private transcriptFocus: OpenTUITranscriptFocusController | undefined;
	private status: OpenTUIStatusView | undefined;
	private unsubscribeSession: (() => void) | undefined;
	private unsubscribeApplicationKeys: (() => void) | undefined;
	private extensionBinding: OpenTUIExtensionBinding | undefined;
	private signalCleanups: Array<() => void> = [];
	private eventTail: Promise<void> = Promise.resolve();
	private submissionTail: Promise<void> = Promise.resolve();
	private readonly interactionTasks = new Set<Promise<void>>();
	private footerData: FooterDataProvider | undefined;
	private sidebarSessions: InteractiveSessionSummary[] = [];
	private sidebarOffset = 0;
	private sidebarHasMore = false;
	private sessionSearchTimer: ReturnType<typeof setTimeout> | undefined;
	private semanticSearchTimer: ReturnType<typeof setTimeout> | undefined;
	private sessionSearchGeneration = 0;
	private sidebarPreviewTarget: string | undefined;
	private sidebarPreviewInFlight: Promise<void> | undefined;
	private memoryStartup: Promise<void> = Promise.resolve();
	private readonly conversationViewStates = new WeakMap<AgentSessionRuntime, ConversationViewState>();
	private autoTrustOnReloadCwd: string | undefined;
	private foregroundGeneration = 0;
	private reviewingPlanProposalId: string | undefined;
	private reviewingQuestionRequestId: string | undefined;
	private allToolDetailsExpanded = false;
	private planInteractionAbort: AbortController | undefined;
	private questionInteractionAbort: AbortController | undefined;
	private initialized = false;
	private stopping = false;
	private cleanupPromise: Promise<void> | undefined;
	private renderTail: Promise<void> = Promise.resolve();
	private resolveShutdown: (() => void) | undefined;
	private readonly shutdown = new Promise<void>((resolve) => {
		this.resolveShutdown = resolve;
	});

	constructor(sessionHost: OpenTUISessionHostContract, options: OpenTUIInteractiveModeOptions = {}) {
		this.sessionHost = sessionHost;
		this.options = options;
		this.autoTrustOnReloadCwd = options.autoTrustOnReloadCwd;
		const memoryOptions: OpenTUIMemoryRuntimeOptions = {
			agentDir: sessionHost.current.services.agentDir,
			cwd: sessionHost.current.session.sessionManager.getCwd(),
			onStatus: (status) => {
				if (status.phase === "preparing") this.setSidebarSearchStatus(status.message);
				if (status.phase === "unavailable") {
					this.setSidebarSearchStatus(status.message ?? "Keyword search · semantic search unavailable");
				}
			},
			onEmbeddingStatus: (status) => this.setSidebarSearchStatus(this.formatSemanticSearchStatus(status)),
			onSearchRefresh: () => {
				const query = this.sidebar?.searchQuery;
				if (query?.trim()) this.scheduleSidebarSearch(query, 0);
			},
		};
		this.memory = options.createMemoryRuntime?.(memoryOptions) ?? new MemoryRuntime(memoryOptions);
		this.commandRouter = new OpenTUICommandRouter({
			host: sessionHost,
			getUI: () => this.extensionBinding?.context,
			onStatus: (message) => {
				this.status?.setMessage(message);
				this.status?.stop();
				this.renderer?.requestRender();
			},
			onFocusConversations: () => this.paneFocus?.focus("sidebar"),
			onQuit: () => this.stop(),
			onReloaded: () => this.maybeSaveImplicitProjectTrustAfterReload(),
			onPresentationChanged: () => this.refreshPresentation(),
		});
		this.sessionHost.setHooks({
			beforeForegroundChange: async (runtime) => this.unbindForeground(runtime),
			foregroundChanged: async (runtime) => this.bindForeground(runtime),
			stateChanged: (structureChanged) => {
				if (structureChanged) void this.refreshSidebar();
				else this.refreshSidebarStates();
			},
			runtimeDisposed: (runtime) => this.conversationViewStates.delete(runtime),
			persistedEntries: async (runtime, entries) => {
				const sessionPath = runtime.session.sessionFile;
				if (!sessionPath) return;
				const manager = runtime.session.sessionManager;
				await this.memory.recordPersistedEntries(
					{ path: sessionPath, id: manager.getSessionId(), name: manager.getSessionName() },
					entries,
				);
			},
			runCompleted: async (runtime, messages) => {
				const sessionPath = runtime.session.sessionFile;
				if (!sessionPath) return;
				const manager = runtime.session.sessionManager;
				await this.memory.recordCompletedRun(
					{ path: sessionPath, id: manager.getSessionId(), name: manager.getSessionName() },
					messages,
				);
			},
		});
	}

	async init(): Promise<void> {
		if (this.initialized) return;
		this.renderer = await (this.options.createRenderer ?? (() => createRenderer()))();
		this.transcriptFactory =
			this.options.createTranscriptFactory?.(this.renderer) ?? new OpenTUITranscriptFactory(this.renderer);
		this.overlayManager = new OverlayManager(this.renderer);
		const settingsManager = this.sessionHost.current.services.settingsManager;
		this.shell = new OpenTUIInteractiveShell(this.renderer, { sidebarWidth: settingsManager.getSidebarWidth() });
		this.shell.onSidebarWidthChange = (width) => settingsManager.setSidebarWidth(width);
		this.renderer.root.add(this.shell.root);

		this.sidebar = new OpenTUISessionSidebar(this.renderer);
		this.shell.setSidebar(this.sidebar.root);

		this.status = new OpenTUIStatusView(this.renderer, "working", "Ready");
		this.status.stop();
		const regions = this.shell.getExtensionRegions();
		this.topBar = new OpenTUITopBar(this.renderer, this.getTopBarState(this.sessionHost.current));
		regions.header.add(this.topBar.root);
		regions.aboveEditor.add(this.status.root);
		this.composer = new OpenTUIComposer(this.renderer, {
			status: this.getComposerStatus(this.sessionHost.current),
			autocompleteProvider: this.options.autocompleteProvider ?? this.commandRouter.createAutocompleteProvider(),
			onSubmit: (text) => {
				this.submissionTail = this.submissionTail.then(async () => this.submit(text));
			},
			onCancel: () => void this.abortOrClear(),
		});
		regions.editor.add(this.composer.root);

		this.paneFocus = new OpenTUIPaneNavigator(this.renderer, (pane) =>
			this.shell?.showPane(pane === "sidebar" ? "sidebar" : "main"),
		);
		const sidebar = this.sidebar;
		const composer = this.composer;
		const paneFocus = this.paneFocus;
		if (!sidebar || !composer || !paneFocus) throw new Error("OpenTUI panes did not initialize");
		this.composer.onFocusRequest = () => this.paneFocus?.focus("composer");
		this.transcriptFocus = new OpenTUITranscriptFocusController(
			this.shell.getTranscriptNode(),
			() => this.renderer?.height ?? 24,
		);
		this.shell.onTranscriptScrollRequest = (delta) => this.transcriptFocus?.scrollByUser(delta);
		this.shell.transcript.onMouseScroll = (event) => {
			const scroll = event.scroll;
			if (scroll && (scroll.direction === "up" || scroll.direction === "down")) {
				this.transcriptFocus?.handleNativeMouseScroll(scroll.direction, scroll.delta);
			}
		};
		this.shell.onTranscriptContentChange = () => {
			if (this.transcriptFocus?.isAutoFollowing()) this.transcriptFocus.followLatest();
		};
		paneFocus.register("sidebar", {
			root: sidebar.root,
			focusTarget: () => sidebar.focusTarget,
			onFocusChange: (focused) => this.sidebar?.setFocused(focused),
		});
		paneFocus.register("composer", {
			root: composer.root,
			focusTarget: () => composer.focusNode,
		});
		const applicationKeyHandler = (event: KeyEvent) => {
			if (this.overlayManager?.active) return;
			if (matchesOpenTUIAction(event, "toggleToolDetails")) {
				event.preventDefault();
				event.stopPropagation();
				this.allToolDetailsExpanded = !this.allToolDetailsExpanded;
				this.transcriptFactory.setAllToolDetailsExpanded(this.allToolDetailsExpanded);
				this.renderer?.requestRender();
				return;
			}
			if (this.paneFocus?.focusedPane === "sidebar") {
				this.sidebar?.handleKey(event);
				return;
			}
			if (this.paneFocus?.focusedPane !== "composer") return;
			if (matchesOpenTUIAction(event, "focusLeft")) {
				event.preventDefault();
				event.stopPropagation();
				this.paneFocus.focus("sidebar");
				return;
			}
			this.composer?.handleKey(event);
			if (matchesOpenTUIAction(event, "clear")) {
				event.preventDefault();
				event.stopPropagation();
				void this.abortOrClear();
			} else if (matchesOpenTUIAction(event, "exit") && !this.composer?.value) {
				event.preventDefault();
				event.stopPropagation();
				this.stop();
			}
		};
		this.renderer.keyInput.on("keypress", applicationKeyHandler);
		this.unsubscribeApplicationKeys = () => this.renderer?.keyInput.off("keypress", applicationKeyHandler);

		// The transcript is a reading surface, not a separate input mode. A click
		// there must leave the composer ready for immediate typing.
		this.shell.onTranscriptFocusRequest = () => this.paneFocus?.focus("composer");
		this.sidebar.onFocusRequest = () => this.paneFocus?.focus("sidebar");
		// Search replaces the sidebar's focus target with a native textarea.
		// Re-run the navigator after the subtree is rebuilt so typing starts
		// immediately and Esc restores focus to the native sidebar root.
		this.sidebar.onSearchStateChange = () => this.paneFocus?.focus("sidebar");
		this.sidebar.onFocusChat = () => this.paneFocus?.focus("composer");
		this.sidebar.onScrollChat = (direction) => this.shell?.scrollTranscript(direction === "up" ? -10 : 10);
		this.sidebar.onActivateSession = (path) => void this.runAction(() => this.sessionHost.activate(path));
		this.sidebar.onPreviewSession = (path) => this.previewSidebarSession(path);
		this.sidebar.onDeleteSession = (path, replacement) =>
			void this.runAction(async () => {
				await this.sessionHost.deleteSession(path, replacement);
				await this.memory.removeSession(path);
				await this.refreshSidebar();
			});
		this.sidebar.onSearchQueryChange = (query) => this.scheduleSidebarSearch(query);
		this.sidebar.onLoadMore = () => void this.loadMoreSessions();
		this.sidebar.onInterrupt = () => void this.abortOrClear();
		this.sidebar.onExit = () => this.stop();

		this.renderer.start();
		await this.bindForeground(this.sessionHost.current);
		await this.refreshSidebar();
		this.memoryStartup = this.memory.start(this.sidebarSessions).catch((error: unknown) => {
			this.setSidebarSearchStatus("Local memory unavailable · conversations remain usable");
			this.showInteractionError(error);
		});
		this.installSignals();
		this.paneFocus.focus("composer");
		this.initialized = true;
		this.schedulePendingInteractions(this.sessionHost.current);
		this.showStartupNotices();
		await this.submitInitialMessages();
	}

	async run(): Promise<void> {
		await this.init();
		await this.shutdown;
		await this.cleanup();
	}

	stop(): void {
		if (this.stopping) return;
		this.stopping = true;
		this.resolveShutdown?.();
		this.resolveShutdown = undefined;
		this.cleanupPromise = this.cleanup();
	}

	async idle(): Promise<void> {
		await this.submissionTail;
		await this.eventTail;
		await Promise.all([...this.interactionTasks]);
		await this.memoryStartup;
	}

	focusComposer(): void {
		this.paneFocus?.focus("composer");
	}

	private async bindForeground(runtime: AgentSessionRuntime): Promise<void> {
		if (!this.shell || !this.renderer) return;
		const generation = this.foregroundGeneration;
		let replaying = true;
		let snapshotRevision = 0;
		let fallbackRevision = 0;
		const pendingEvents: RuntimeEventEnvelope[] = [];
		const enqueueEvent = (envelope: RuntimeEventEnvelope) => {
			if (replaying) {
				pendingEvents.push(envelope);
				return;
			}
			if (envelope.revision <= snapshotRevision) return;
			snapshotRevision = envelope.revision;
			this.queueSessionEvent(runtime, envelope.event, generation);
		};
		this.unsubscribeSession = this.sessionHost.subscribeRuntime
			? this.sessionHost.subscribeRuntime(runtime, enqueueEvent)
			: runtime.session.subscribe((event) =>
					enqueueEvent({ runtime, revision: ++fallbackRevision, generationId: undefined, event }),
				);
		this.shell.clearTranscript();
		const history: string[] = [];
		for (const entry of runtime.session.sessionManager.getEntries()) {
			if (entry.type === "message" && entry.message.role === "user") history.unshift(messageText(entry.message));
		}
		this.composer?.setHistory(history);
		const viewState = this.conversationViewStates.get(runtime);
		this.composer?.setValue(viewState?.draft ?? "");
		this.shell.getTranscriptNode().scrollTop = viewState?.scrollTop ?? Number.MAX_SAFE_INTEGER;
		this.extensionBinding = this.options.bindExtensionUI
			? this.options.bindExtensionUI(runtime, this.renderer)
			: this.createDefaultExtensionBinding(runtime, this.renderer);
		if (this.extensionBinding) {
			await runtime.session.bindExtensions({
				uiV2Context: this.extensionBinding.context,
				mode: "tui",
				abortHandler: () => void this.abortOrClear(),
				commandContextActions: {
					waitForIdle: () => runtime.session.waitForIdle(),
					newSession: async (options) => {
						await runtime.newSession(options);
						return { cancelled: false };
					},
					fork: async (entryId, options) => {
						const result = await runtime.fork(entryId, options);
						if (!result.cancelled && result.selectedText && !this.composer?.value.trim()) {
							this.composer?.setValue(result.selectedText);
						}
						return { cancelled: result.cancelled };
					},
					navigateTree: async (targetId, options) => {
						const result = await runtime.session.navigateTree(targetId, options);
						if (!result.cancelled && result.editorText && !this.composer?.value.trim()) {
							this.composer?.setValue(result.editorText);
						}
						return { cancelled: result.cancelled };
					},
					switchSession: async (sessionPath, options) => runtime.switchSession(sessionPath, options),
					reload: async () => {
						await runtime.session.reload();
						this.maybeSaveImplicitProjectTrustAfterReload();
					},
				},
				shutdownHandler: async () => this.stop(),
				onError: (error) => this.showInteractionError(error.error),
			});
		}
		this.composer?.setAutocompleteProvider(
			this.options.autocompleteProvider ?? this.commandRouter.createAutocompleteProvider(runtime.cwd),
		);
		this.composer?.updateStatus(this.getComposerStatus(runtime));
		const historySnapshot = await this.renderTranscript(
			runtime,
			generation,
			() =>
				this.sessionHost.getRuntimeStreamSnapshot?.(runtime) ?? {
					revision: fallbackRevision,
					generationId: undefined,
					liveEvents: [],
					liveEventEnvelopes: [],
				},
		);
		if (!this.isCurrentForeground(runtime, generation)) return;
		// Read again after history rendering. Persistence acknowledgements may have
		// removed events that are now represented by durable SessionEntries.
		const replaySnapshot = this.sessionHost.getRuntimeStreamSnapshot?.(runtime) ?? {
			revision: fallbackRevision,
			generationId: undefined,
			liveEvents: [],
			liveEventEnvelopes: [],
		};
		const activeStreamGeneration = replaySnapshot.generationId ?? historySnapshot.generationId;
		const belongsToActiveStream = (envelope: RuntimeEventEnvelope): boolean =>
			activeStreamGeneration === undefined || envelope.generationId === activeStreamGeneration;
		const replayEnvelopes = [
			...historySnapshot.liveEventEnvelopes.filter(belongsToActiveStream),
			...replaySnapshot.liveEventEnvelopes.filter(belongsToActiveStream),
			...pendingEvents.filter(
				(envelope) => envelope.revision > historySnapshot.revision && belongsToActiveStream(envelope),
			),
		].sort((left, right) => left.revision - right.revision);
		const replayedRevisions = new Set<string>();
		for (const envelope of replayEnvelopes) {
			const replayKey = `${envelope.generationId ?? "legacy"}:${envelope.revision}`;
			if (replayedRevisions.has(replayKey)) continue;
			replayedRevisions.add(replayKey);
			await this.handleSessionEvent(runtime, envelope.event, generation);
		}
		if (replayEnvelopes.length === 0 && historySnapshot.liveEvents.length === 0) {
			for (const event of replaySnapshot.liveEvents) await this.handleSessionEvent(runtime, event, generation);
		}
		snapshotRevision = Math.max(replaySnapshot.revision, ...pendingEvents.map((envelope) => envelope.revision));
		replaying = false;
		this.renderer.requestRender();
		this.topBar?.update(this.getTopBarState(runtime));
		this.rememberActiveConversation(runtime);
		if (this.initialized) {
			this.schedulePendingInteractions(runtime, generation);
			if (!this.overlayManager?.active) this.paneFocus?.focus("composer");
		}
	}

	private queueSessionEvent(runtime: AgentSessionRuntime, event: AgentSessionEvent, generation: number): void {
		this.eventTail = this.eventTail
			.then(async () => this.handleSessionEvent(runtime, event, generation))
			.catch((error: unknown) => this.showInteractionError(error));
	}

	private async unbindForeground(runtime?: AgentSessionRuntime): Promise<void> {
		if (runtime && this.composer && this.shell) {
			this.conversationViewStates.set(runtime, {
				draft: this.composer.value,
				scrollTop: this.shell.getTranscriptNode().scrollTop,
			});
		}
		this.foregroundGeneration++;
		this.planInteractionAbort?.abort();
		this.planInteractionAbort = undefined;
		this.questionInteractionAbort?.abort();
		this.questionInteractionAbort = undefined;
		this.unsubscribeSession?.();
		this.unsubscribeSession = undefined;
		this.extensionBinding?.dispose();
		this.extensionBinding = undefined;
		runtime?.session.parkExtensionUI?.();
		this.footerData?.dispose();
		this.footerData = undefined;
	}

	private createDefaultExtensionBinding(runtime: AgentSessionRuntime, renderer: CliRenderer): OpenTUIExtensionBinding {
		if (!this.shell || !this.composer || !this.overlayManager) throw new Error("OpenTUI shell is not mounted");
		this.footerData = new FooterDataProvider(runtime.cwd);
		const host = new OpenTUIExtensionHost({
			renderer,
			overlayManager: this.overlayManager,
			regions: this.shell.getExtensionRegions(),
			editor: {
				getText: () => this.composer?.value ?? "",
				setText: (text) => this.composer?.setValue(text),
				insertText: (text) => this.composer?.setValue(`${this.composer.value}${text}`),
				focusTarget: this.composer.focusNode,
			},
			footerData: this.footerData,
			onNotify: (message) => {
				this.status?.setMessage(message);
				this.status?.stop();
			},
			onTitle: (title) => process.stdout.write(`\u001b]0;${title}\u0007`),
		});
		return {
			context: host.context,
			getToolRenderer: (name) => host.getToolRenderer(name),
			dispose: () => host.dispose(),
		};
	}

	private async renderTranscript(
		runtime: AgentSessionRuntime,
		generation = this.foregroundGeneration,
		readSnapshot: () => RuntimeStreamSnapshot = () =>
			this.sessionHost.getRuntimeStreamSnapshot?.(runtime) ?? {
				revision: 0,
				generationId: undefined,
				liveEvents: [],
				liveEventEnvelopes: [],
			},
	): Promise<RuntimeStreamSnapshot> {
		let historySnapshot = readSnapshot();
		const render = this.renderTail
			.catch(() => {})
			.then(async () => {
				if (!this.shell || !this.isCurrentForeground(runtime, generation)) return;
				const transcriptFactory =
					this.options.createTranscriptFactory?.(this.renderer!) ??
					new OpenTUITranscriptFactory(this.renderer!, {
						showImages: runtime.services.settingsManager?.getShowImages?.() ?? true,
						hideThinkingBlock: runtime.services.settingsManager?.getHideThinkingBlock?.() ?? false,
					});
				// A caller may intentionally reuse one factory across foreground
				// sessions. Clear call-scoped tool bookkeeping before replay so
				// reused tool ids cannot update a previous session's native roots.
				transcriptFactory.reset();
				transcriptFactory.setResolvers({
					cwd: runtime.cwd,
					getToolRenderer: (name) =>
						this.extensionBinding?.getToolRenderer?.(name) ?? runtime.session.getToolDefinition?.(name)?.renderV2,
					getMessageView: (type) => runtime.session.extensionRunner.getMessageView(type),
					getEntryView: (type) => runtime.session.extensionRunner.getEntryView(type),
					onError: (error, surface) => {
						const message = error instanceof Error ? error.message : String(error);
						runtime.session.extensionRunner.emitError({
							extensionPath: "<ui-renderer>",
							event: surface,
							error: message,
							stack: error instanceof Error ? error.stack : undefined,
						});
					},
				});
				transcriptFactory.setAllToolDetailsExpanded(this.allToolDetailsExpanded);
				historySnapshot = readSnapshot();
				const entries = runtime.session.sessionManager.getEntries();
				const items = await transcriptFactory.createSessionEntries(entries);
				if (!this.shell || !this.isCurrentForeground(runtime, generation)) return;
				this.shell.clearTranscript();
				this.welcome = undefined;
				this.transcriptFactory = transcriptFactory;
				for (const item of items) this.shell.appendTranscript(item.root);
				const hasConversationMessages = entries.some(
					(entry) =>
						entry.type === "message" && (entry.message.role === "user" || entry.message.role === "assistant"),
				);
				if (!hasConversationMessages) {
					this.welcome = new OpenTUIWelcome(this.renderer!, { workspace: runtime.cwd });
					this.shell.appendTranscript(this.welcome.root);
				}
			});
		this.renderTail = render;
		await render;
		return historySnapshot;
	}

	private async refreshPresentation(): Promise<void> {
		this.shell?.updateTheme(theme);
		this.sidebar?.updateTheme(theme);
		this.composer?.updateTheme(theme);
		this.status?.updateTheme(theme);
		this.topBar?.updateTheme(theme);
		this.topBar?.update(this.getTopBarState(this.sessionHost.current));
		const runtime = this.sessionHost.current;
		const refresh = async (target: AgentSessionRuntime) => {
			if (target !== this.sessionHost.current || this.stopping || !this.shell) return;
			// Serialize the destructive transcript replacement with live event handling.
			// Events arriving after this task is queued are appended behind it by
			// queueSessionEvent and therefore apply to the new transcript factory.
			const refreshTask = this.eventTail.then(async () => {
				if (!this.shell || target !== this.sessionHost.current || this.stopping) return;
				const transcript = this.shell.getTranscriptNode();
				const scrollTop = transcript.scrollTop;
				const snapshot = this.sessionHost.getRuntimeStreamSnapshot?.(target) ?? {
					revision: 0,
					generationId: undefined,
					liveEvents: [],
					liveEventEnvelopes: [],
				};
				await this.renderTranscript(target, this.foregroundGeneration, () => snapshot);
				if (!this.isCurrentForeground(target, this.foregroundGeneration)) return;
				for (const envelope of snapshot.liveEventEnvelopes) {
					await this.handleSessionEvent(target, envelope.event, this.foregroundGeneration);
				}
				if (snapshot.liveEventEnvelopes.length === 0) {
					for (const event of snapshot.liveEvents) {
						await this.handleSessionEvent(target, event, this.foregroundGeneration);
					}
				}
				transcript.scrollTo(scrollTop);
			});
			this.eventTail = refreshTask.catch((error: unknown) => this.showInteractionError(error));
			await refreshTask;
		};
		if (this.sessionHost.refreshForeground) await this.sessionHost.refreshForeground(refresh);
		else await refresh(runtime);
		this.renderer?.requestRender();
	}

	private async handleSessionEvent(
		runtime: AgentSessionRuntime,
		event: AgentSessionEvent,
		generation = this.foregroundGeneration,
	): Promise<void> {
		if (!this.isCurrentForeground(runtime, generation) || !this.shell) return;
		if (event.type === "message_start" && event.message.role === "user") {
			this.welcome?.dismiss();
			this.welcome = undefined;
		}
		switch (event.type) {
			case "agent_start":
				this.status?.setMessage("Working...");
				break;
			case "agent_end":
			case "agent_settled":
				this.status?.setMessage("Ready");
				this.status?.stop();
				break;
			case "compaction_start":
				this.status?.setMessage("Compacting context...");
				break;
			case "auto_retry_start":
				this.status?.setMessage(`Retry ${event.attempt}/${event.maxAttempts}`);
				break;
		}
		const mutation = await this.transcriptFactory.handleEvent(event);
		if (mutation.type === "append") this.shell.appendTranscript(mutation.item.root);
		if (this.transcriptFocus?.isAutoFollowing()) this.transcriptFocus.followLatest();
		if (event.type === "session_info_changed" || event.type === "thinking_level_changed") {
			this.topBar?.update(this.getTopBarState(runtime));
		}
		if (
			event.type === "agent_start" ||
			event.type === "agent_end" ||
			event.type === "agent_settled" ||
			event.type === "thinking_level_changed"
		) {
			this.composer?.updateStatus(this.getComposerStatus(runtime));
		}
		this.renderer?.requestRender();
		if (event.type === "question_asked") {
			this.launchInteraction(() => this.reviewPendingQuestion(runtime, this.foregroundGeneration));
		} else if (event.type === "agent_settled") {
			this.launchInteraction(() => this.reviewPendingPlan(runtime, this.foregroundGeneration));
		} else if (event.type === "plan_decided" && this.reviewingPlanProposalId === event.proposal.id) {
			this.planInteractionAbort?.abort();
		} else if (
			(event.type === "question_answered" || event.type === "question_cancelled") &&
			this.reviewingQuestionRequestId === event.requestId
		) {
			this.questionInteractionAbort?.abort();
		}
	}

	private schedulePendingInteractions(runtime: AgentSessionRuntime, generation = this.foregroundGeneration): void {
		this.launchInteraction(async () => {
			await this.reviewPendingQuestion(runtime, generation);
			await this.reviewPendingPlan(runtime, generation);
		});
	}

	private launchInteraction(interaction: () => Promise<void>): void {
		const task = interaction()
			.catch((error: unknown) => this.showInteractionError(error))
			.finally(() => this.interactionTasks.delete(task));
		this.interactionTasks.add(task);
	}

	private isCurrentForeground(runtime: AgentSessionRuntime, generation: number): boolean {
		return runtime === this.sessionHost.current && generation === this.foregroundGeneration && !this.stopping;
	}

	private getInteractiveUI(): ExtensionUIV2Context | undefined {
		const ui = this.extensionBinding?.context;
		return ui?.available ? ui : undefined;
	}

	private async reviewPendingPlan(runtime: AgentSessionRuntime, generation: number): Promise<void> {
		const initialState = runtime.session.planState;
		if (initialState.status !== "awaitingApproval" || this.reviewingPlanProposalId !== undefined) return;
		const ui = this.getInteractiveUI();
		if (!ui) {
			this.showInteractionError("Plan review requires the interactive OpenTUI dialog host");
			return;
		}

		const proposal = initialState.proposal;
		const controller = new AbortController();
		this.reviewingPlanProposalId = proposal.id;
		this.planInteractionAbort = controller;
		try {
			while (this.isPendingPlan(runtime, generation, proposal)) {
				const choice = await ui.dialogs.select({
					title: `Plan v${proposal.version} · what next?`,
					options: [
						{ value: "approve", label: "Execute plan", description: "Approve and begin implementation" },
						{ value: "revise", label: "Revise plan", description: "Send feedback and request a replacement" },
						{ value: "cancel", label: "Cancel plan", description: "Leave Plan mode without executing" },
					],
					signal: controller.signal,
				});
				if (!this.isPendingPlan(runtime, generation, proposal) || controller.signal.aborted) return;
				if (choice === "approve") {
					await runtime.session.approvePlan(proposal.id);
					return;
				}
				if (choice === "cancel" || choice === undefined) {
					runtime.session.cancelPlan(proposal.id);
					return;
				}

				const feedback = await ui.editor.open({
					title: `Revise plan v${proposal.version}`,
					placeholder: "Describe what should change",
					multiline: true,
					signal: controller.signal,
				});
				if (!this.isPendingPlan(runtime, generation, proposal) || controller.signal.aborted) return;
				if (feedback?.trim()) {
					await runtime.session.revisePlan(proposal.id, feedback);
					return;
				}
			}
		} catch (error) {
			this.showInteractionError(error);
		} finally {
			if (this.planInteractionAbort === controller) this.planInteractionAbort = undefined;
			if (this.reviewingPlanProposalId === proposal.id) this.reviewingPlanProposalId = undefined;
			if (this.isCurrentForeground(runtime, generation)) this.focusComposer();
		}
	}

	private isPendingPlan(runtime: AgentSessionRuntime, generation: number, proposal: PlanProposal): boolean {
		if (!this.isCurrentForeground(runtime, generation)) return false;
		const state = runtime.session.planState;
		return state.status === "awaitingApproval" && state.proposal.id === proposal.id;
	}

	private async reviewPendingQuestion(runtime: AgentSessionRuntime, generation: number): Promise<void> {
		const state = runtime.session.questionState;
		if (state.status !== "awaitingAnswer" || this.reviewingQuestionRequestId !== undefined) return;
		const request = state.request;
		const ui = this.getInteractiveUI();
		if (!ui) {
			if (this.isPendingQuestion(runtime, generation, request)) runtime.session.cancelQuestion(request.id, "no_ui");
			this.showInteractionError("Structured question requires the interactive OpenTUI dialog host");
			return;
		}

		const controller = new AbortController();
		this.reviewingQuestionRequestId = request.id;
		this.questionInteractionAbort = controller;
		try {
			const answers = await this.collectQuestionAnswers(ui, request, controller.signal);
			if (!this.isPendingQuestion(runtime, generation, request) || controller.signal.aborted) return;
			if (answers) runtime.session.answerQuestion(request.id, answers);
			else runtime.session.cancelQuestion(request.id, "user");
		} catch (error) {
			this.showInteractionError(error);
		} finally {
			if (this.questionInteractionAbort === controller) this.questionInteractionAbort = undefined;
			if (this.reviewingQuestionRequestId === request.id) this.reviewingQuestionRequestId = undefined;
			if (this.isCurrentForeground(runtime, generation)) this.focusComposer();
		}
	}

	private isPendingQuestion(runtime: AgentSessionRuntime, generation: number, request: QuestionRequest): boolean {
		if (!this.isCurrentForeground(runtime, generation)) return false;
		const state = runtime.session.questionState;
		return state.status === "awaitingAnswer" && state.request.id === request.id;
	}

	private async collectQuestionAnswers(
		ui: ExtensionUIV2Context,
		request: QuestionRequest,
		signal: AbortSignal,
	): Promise<QuestionAnswer[] | undefined> {
		const answers: QuestionAnswer[] = [];
		for (let questionIndex = 0; questionIndex < request.questions.length; questionIndex++) {
			const question = request.questions[questionIndex]!;
			const title = `${questionIndex + 1}/${request.questions.length} · ${question.header}: ${question.question}`;
			if (!question.multiSelect) {
				const choice = await ui.dialogs.select({
					title,
					options: [
						...question.options.map((option, optionIndex) => ({
							value: `option:${optionIndex}`,
							label: option.label,
							description: option.preview ? `${option.description} · ${option.preview}` : option.description,
						})),
						{ value: CUSTOM_ANSWER, label: "Custom answer", description: "Enter a different answer" },
					],
					signal,
				});
				if (choice === undefined) return undefined;
				if (choice === CUSTOM_ANSWER) {
					const custom = await ui.editor.open({
						title,
						placeholder: "Enter your answer",
						multiline: true,
						signal,
					});
					if (custom === undefined) return undefined;
					const value = custom.trim();
					if (!value) {
						questionIndex--;
						continue;
					}
					answers.push({ questionIndex, question: question.question, kind: "custom", answer: value });
					continue;
				}
				const optionIndex = Number(choice.slice("option:".length));
				const option = question.options[optionIndex];
				if (!option) throw new Error("The selected question option is no longer available.");
				answers.push({ questionIndex, question: question.question, kind: "option", answer: option.label });
				continue;
			}

			const selected = new Set<number>();
			while (true) {
				const choice = await ui.dialogs.select({
					title,
					options: [
						...question.options.map((option, optionIndex) => ({
							value: `option:${optionIndex}`,
							label: `${selected.has(optionIndex) ? "[x]" : "[ ]"} ${option.label}`,
							description: option.preview ? `${option.description} · ${option.preview}` : option.description,
						})),
						{
							value: CUSTOM_ANSWER,
							label: "Custom answer",
							description: "Replace selections with a written answer",
						},
						{
							value: SUBMIT_SELECTION,
							label: "Submit selection",
							description: selected.size ? `${selected.size} selected` : "Select at least one option",
							disabled: selected.size === 0,
						},
					],
					signal,
				});
				if (choice === undefined) return undefined;
				if (choice === CUSTOM_ANSWER) {
					const custom = await ui.editor.open({
						title,
						placeholder: "Enter your answer",
						multiline: true,
						signal,
					});
					if (custom === undefined) return undefined;
					const value = custom.trim();
					if (!value) continue;
					answers.push({ questionIndex, question: question.question, kind: "custom", answer: value });
					break;
				}
				if (choice === SUBMIT_SELECTION) {
					answers.push({
						questionIndex,
						question: question.question,
						kind: "multi",
						answer: null,
						selected: [...selected].map((optionIndex) => question.options[optionIndex]!.label),
					});
					break;
				}
				const optionIndex = Number(choice.slice("option:".length));
				if (!question.options[optionIndex]) throw new Error("The selected question option is no longer available.");
				if (selected.has(optionIndex)) selected.delete(optionIndex);
				else selected.add(optionIndex);
			}
		}
		return answers;
	}

	private showInteractionError(error: unknown): void {
		this.status?.setMessage(error instanceof Error ? error.message : String(error));
		this.status?.stop();
		this.renderer?.requestRender();
	}

	private getTopBarState(runtime: AgentSessionRuntime) {
		const model = runtime.session.model;
		return {
			conversation: runtime.session.sessionName?.trim() || "New conversation",
			workspace: basename(runtime.cwd) || runtime.cwd,
			model: model ? `${model.provider}/${model.id}` : "No model",
			thinking: runtime.session.thinkingLevel ?? "off",
		};
	}

	private getComposerStatus(runtime: AgentSessionRuntime): OpenTUIComposerStatus {
		const model = runtime.session.model;
		const usage = runtime.session.getContextUsage?.();
		const remaining = usage?.percent === null || usage?.percent === undefined ? undefined : 100 - usage.percent;
		const sessionPath = runtime.session.sessionFile;
		const throughput = sessionPath
			? this.sessionHost.getSessionPresentation(sessionPath).throughputTokensPerSecond
			: undefined;
		return {
			cwd: basename(runtime.cwd) || runtime.cwd,
			model: model ? `${model.provider}/${model.id}` : "No model",
			thinking: runtime.session.thinkingLevel ?? "off",
			contextRemaining: remaining === undefined ? "--" : `${Math.max(0, Math.round(remaining))}%`,
			foregroundThroughput: throughput ? `${throughput.toFixed(1)} t/s` : "",
		};
	}

	private async submit(text: string, options?: PromptOptions): Promise<void> {
		const value = text.trim();
		if (!value) return;
		const runtime = this.sessionHost.current;
		this.composer?.addHistoryEntry(value);
		this.composer?.setValue("");
		await this.runAction(async () => {
			const routed = await this.commandRouter.route(value);
			if (routed.handled) return;
			await this.sessionHost.prompt(runtime, value, {
				source: "interactive",
				...(runtime.session.isStreaming ? { streamingBehavior: "steer" as const } : {}),
				...options,
			});
		});
	}

	private async submitInitialMessages(): Promise<void> {
		if (this.options.initialMessage) {
			await this.submit(this.options.initialMessage, { images: this.options.initialImages });
		}
		for (const message of this.options.initialMessages ?? []) await this.submit(message);
	}

	private async abortOrClear(): Promise<void> {
		const session = this.sessionHost.current.session;
		if (session.isStreaming || session.isCompacting || session.isBashRunning) {
			await this.runAction(() => session.abort());
			return;
		}
		this.composer?.setValue("");
	}

	private async refreshSidebar(): Promise<void> {
		if (!this.sidebar) return;
		const page = await this.sessionHost.listPage(0, SIDEBAR_PAGE_SIZE);
		this.sidebarSessions = page.sessions;
		this.sidebarOffset = page.nextOffset;
		this.sidebarHasMore = page.hasMore;
		this.sidebar.setSessions(this.sidebarSessions);
		const query = this.sidebar.searchQuery;
		if (query?.trim()) this.scheduleSidebarSearch(query, 0);
		this.renderer?.requestRender();
	}

	private refreshSidebarStates(): void {
		this.sidebarSessions = this.sidebarSessions.map((session) => ({
			...session,
			...this.sessionHost.getSessionPresentation(session.path),
		}));
		this.sidebar?.setSessions(this.sidebarSessions);
		this.composer?.updateStatus(this.getComposerStatus(this.sessionHost.current));
		this.renderer?.requestRender();
	}

	private async loadMoreSessions(): Promise<void> {
		if (!this.sidebar || !this.sidebarHasMore) return;
		const page = await this.sessionHost.listPage(this.sidebarOffset, SIDEBAR_PAGE_SIZE);
		this.sidebarSessions = [...this.sidebarSessions, ...page.sessions];
		this.sidebarOffset = page.nextOffset;
		this.sidebarHasMore = page.hasMore;
		this.sidebar.setSessions(this.sidebarSessions);
		this.renderer?.requestRender();
	}

	private previewSidebarSession(sessionPath: string): void {
		this.sidebarPreviewTarget = sessionPath;
		if (this.sidebarPreviewInFlight) return;
		this.sidebarPreviewInFlight = this.drainSidebarPreviews().finally(() => {
			this.sidebarPreviewInFlight = undefined;
			if (this.sidebarPreviewTarget) this.previewSidebarSession(this.sidebarPreviewTarget);
		});
	}

	private async drainSidebarPreviews(): Promise<void> {
		while (this.sidebarPreviewTarget) {
			const sessionPath = this.sidebarPreviewTarget;
			this.sidebarPreviewTarget = undefined;
			await this.runAction(() => this.sessionHost.activate(sessionPath));
			if (this.sidebar?.searchActive) this.paneFocus?.focus("sidebar");
		}
	}

	private scheduleSidebarSearch(query: string, delay = LEXICAL_SEARCH_DELAY_MS): void {
		if (this.sessionSearchTimer) clearTimeout(this.sessionSearchTimer);
		if (this.semanticSearchTimer) clearTimeout(this.semanticSearchTimer);
		this.sessionSearchTimer = undefined;
		this.semanticSearchTimer = undefined;
		const generation = ++this.sessionSearchGeneration;
		if (!query.trim()) {
			this.sidebar?.setSearchResults(undefined);
			this.sidebar?.setSearchStatus(undefined);
			this.renderer?.requestRender();
			return;
		}
		this.sessionSearchTimer = setTimeout(() => void this.runSidebarSearch(query, generation), delay);
		this.semanticSearchTimer = setTimeout(() => {
			this.setSidebarSearchStatus("Local semantic search · preparing model...");
			void this.runSemanticSidebarSearch(query, generation);
		}, SEMANTIC_SEARCH_DELAY_MS);
	}

	private async runSidebarSearch(query: string, generation: number): Promise<void> {
		try {
			const results = await this.memory.search(query, this.sidebarSessions);
			if (!this.isCurrentSidebarSearch(query, generation)) return;
			await this.includeSidebarSearchSessions(results.map((result) => result.sessionPath));
			if (!this.isCurrentSidebarSearch(query, generation)) return;
			this.sidebar?.setSearchResults(results);
			this.renderer?.requestRender();
		} catch {
			if (generation !== this.sessionSearchGeneration) return;
			this.sidebar?.setSearchResults([]);
			this.renderer?.requestRender();
		}
	}

	private async runSemanticSidebarSearch(query: string, generation: number): Promise<void> {
		try {
			const results = await this.memory.searchSemantic(query, this.sidebarSessions);
			if (!this.isCurrentSidebarSearch(query, generation)) return;
			await this.includeSidebarSearchSessions(results.map((result) => result.sessionPath));
			if (!this.isCurrentSidebarSearch(query, generation)) return;
			this.sidebar?.setSearchResults(results);
			this.setSidebarSearchStatus(undefined);
		} catch {
			if (generation === this.sessionSearchGeneration) {
				this.setSidebarSearchStatus("Local semantic search unavailable · lexical results shown");
			}
		}
	}

	private isCurrentSidebarSearch(query: string, generation: number): boolean {
		return generation === this.sessionSearchGeneration && query === this.sidebar?.searchQuery;
	}

	private async includeSidebarSearchSessions(sessionPaths: readonly string[]): Promise<void> {
		const known = new Set(this.sidebarSessions.map((session) => resolve(session.path)));
		const missing = sessionPaths.filter((sessionPath) => !known.has(resolve(sessionPath)));
		if (missing.length === 0) return;
		this.sidebarSessions.push(...(await this.sessionHost.getSessionSummaries(missing)));
		this.sidebar?.setSessions(this.sidebarSessions);
	}

	private setSidebarSearchStatus(status: string | undefined): void {
		this.sidebar?.setSearchStatus(status);
		this.renderer?.requestRender();
	}

	private formatSemanticSearchStatus(status: LocalEmbeddingStatus | undefined): string | undefined {
		if (!status) return undefined;
		if (status.phase === "ready") return "Local semantic search · updating results...";
		if (status.phase === "loading") return "Local semantic search · loading local model...";
		if (!status.totalBytes || status.totalBytes <= 0 || status.loadedBytes === undefined) {
			return "Local semantic search · downloading model...";
		}
		const percent = Math.min(100, Math.floor((status.loadedBytes / status.totalBytes) * 100));
		return `Local semantic search · downloading ${percent}%`;
	}

	private rememberActiveConversation(runtime: AgentSessionRuntime): void {
		const sessionPath = runtime.session.sessionFile;
		if (!sessionPath) return;
		const manager = runtime.session.sessionManager;
		rememberLastActiveConversation(manager.getCwd(), manager.getSessionDir(), sessionPath, runtime.services.agentDir);
	}

	private showStartupNotices(): void {
		const notices: string[] = [];
		if (this.options.migratedProviders?.length) {
			notices.push(`Migrated credentials to auth.json: ${this.options.migratedProviders.join(", ")}`);
		}
		if (this.options.modelFallbackMessage) notices.push(this.options.modelFallbackMessage);
		if (this.options.verbose && this.sessionHost.current.session.scopedModels.length > 0) {
			notices.push(
				`Scoped models: ${this.sessionHost.current.session.scopedModels
					.map(
						({ model, thinkingLevel }) =>
							`${model.provider}/${model.id}${thinkingLevel ? `:${thinkingLevel}` : ""}`,
					)
					.join(", ")}`,
			);
		}
		if (notices.length === 0) return;
		this.status?.setMessage(notices.join("\n"));
		this.status?.stop();
		this.renderer?.requestRender();
	}

	private maybeSaveImplicitProjectTrustAfterReload(): void {
		const runtime = this.sessionHost.current;
		if (this.autoTrustOnReloadCwd !== runtime.cwd) return;
		if (!runtime.services.settingsManager.isProjectTrusted() || !hasTrustRequiringProjectResources(runtime.cwd)) {
			return;
		}
		const trustStore = new ProjectTrustStore(runtime.services.agentDir);
		try {
			if (trustStore.get(runtime.cwd) === null) trustStore.set(runtime.cwd, true);
			this.autoTrustOnReloadCwd = undefined;
		} catch (error) {
			this.showInteractionError(
				`Could not save project trust after reload: ${error instanceof Error ? error.message : String(error)}`,
			);
		}
	}

	private async runAction(action: () => Promise<unknown>): Promise<void> {
		try {
			await action();
		} catch (error) {
			this.status?.setMessage(error instanceof Error ? error.message : String(error));
			this.status?.stop();
			this.renderer?.requestRender();
		}
	}

	private installSignals(): void {
		if (this.options.installSignalHandlers === false) return;
		const onSigint = () => void this.abortOrClear();
		const onSigterm = () => this.stop();
		process.on("SIGINT", onSigint);
		process.on("SIGTERM", onSigterm);
		this.signalCleanups.push(
			() => process.off("SIGINT", onSigint),
			() => process.off("SIGTERM", onSigterm),
		);
	}

	private async cleanup(): Promise<void> {
		if (this.cleanupPromise) return this.cleanupPromise;
		this.cleanupPromise = (async () => {
			await this.unbindForeground(this.sessionHost.current);
			if (this.sessionSearchTimer) clearTimeout(this.sessionSearchTimer);
			if (this.semanticSearchTimer) clearTimeout(this.semanticSearchTimer);
			this.sidebarPreviewTarget = undefined;
			for (const cleanup of this.signalCleanups.splice(0)) cleanup();
			this.unsubscribeApplicationKeys?.();
			this.unsubscribeApplicationKeys = undefined;
			this.paneFocus?.dispose();
			// OverlayManager owns the native key/resize listeners and the overlay
			// layer. Dispose it before tearing down the shell or renderer so no
			// late async dialog can attach into a destroyed native tree.
			await this.overlayManager?.dispose();
			this.overlayManager = undefined;
			this.topBar?.dispose();
			this.sidebar?.dispose();
			this.shell?.dispose();
			this.composer?.destroy();
			this.renderer?.stop();
			this.renderer?.destroy();
			await this.sessionHost.disposeAll();
			await this.memory.dispose();
		})();
		return this.cleanupPromise;
	}
}
