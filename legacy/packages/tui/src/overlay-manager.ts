import {
	BoxRenderable,
	CliRenderEvents,
	type CliRenderer,
	type ColorInput,
	type KeyEvent,
	type Renderable,
	RenderableEvents,
} from "@opentui/core";

export type OverlayAnchor =
	| "center"
	| "top-left"
	| "top-center"
	| "top-right"
	| "left-center"
	| "right-center"
	| "bottom-left"
	| "bottom-center"
	| "bottom-right";

export type OverlayCloseReason = "close" | "abort" | "timeout" | "dispose" | "content-destroyed" | "factory-error";
export type OverlayLifecycleState = "opening" | "open" | "hidden" | "closing" | "closed";
export type OverlayDimension = number | "auto" | `${number}%`;

export interface OverlayViewport {
	readonly width: number;
	readonly height: number;
}

export interface OverlayLayout {
	anchor?: OverlayAnchor;
	width?: OverlayDimension;
	height?: OverlayDimension;
	minWidth?: OverlayDimension;
	minHeight?: OverlayDimension;
	maxWidth?: OverlayDimension;
	maxHeight?: OverlayDimension;
	margin?: number | `${number}%`;
	zIndex?: number;
	backdropColor?: ColorInput;
}

export interface OverlayDescriptor<Root extends Renderable = Renderable> {
	readonly root: Root;
	readonly focusTarget?: Renderable | null;
}

export interface OverlayHandle<Root extends Renderable = Renderable> {
	readonly id: number;
	readonly root: Root;
	readonly wrapper: BoxRenderable;
	readonly state: Exclude<OverlayLifecycleState, "opening">;
	readonly hidden: boolean;
	focus(): void;
	hide(): void;
	show(): void;
	updateLayout(layout: OverlayLayout | ((viewport: OverlayViewport) => OverlayLayout)): void;
	close(reason?: OverlayCloseReason): Promise<void>;
}

export interface OpenOverlayOptions<Root extends Renderable> {
	/** Explicit application or parent-overlay target restored after close. */
	restoreFocus: Renderable | null;
	layout?: OverlayLayout | ((viewport: OverlayViewport) => OverlayLayout);
	signal?: AbortSignal;
	timeoutMs?: number;
	onKey?: (event: KeyEvent, handle: OverlayHandle<Root>) => boolean;
	onClose?: (reason: OverlayCloseReason) => void | Promise<void>;
}

export type OverlayFactory<Root extends Renderable> = (
	renderer: CliRenderer,
) => OverlayDescriptor<Root> | Promise<OverlayDescriptor<Root>>;

export class OverlayOpenCancelledError extends Error {
	readonly reason: "abort" | "timeout" | "dispose";

	constructor(reason: "abort" | "timeout" | "dispose") {
		super(`Overlay opening was cancelled: ${reason}`);
		this.name = "OverlayOpenCancelledError";
		this.reason = reason;
	}
}

interface OverlayEntry {
	readonly handle: InternalOverlayHandle;
	readonly restoreFocus: Renderable | null;
	readonly focusTarget: Renderable | null;
	readonly onKey: ((event: KeyEvent, handle: OverlayHandle) => boolean) | undefined;
	readonly onClose: ((reason: OverlayCloseReason) => void | Promise<void>) | undefined;
	readonly abortSignal: AbortSignal | undefined;
	readonly abortListener: (() => void) | undefined;
	readonly contentDestroyedListener: () => void;
	layout: OverlayLayout | ((viewport: OverlayViewport) => OverlayLayout);
	timeout: ReturnType<typeof setTimeout> | undefined;
}

interface OpeningEntry {
	readonly id: number;
	state: "opening" | "closing" | "closed";
	cancel(reason: "abort" | "timeout" | "dispose"): Promise<void>;
}

const liveManagers = new WeakSet<CliRenderer>();

interface InternalOverlayHandle extends OverlayHandle {
	setState(state: Exclude<OverlayLifecycleState, "opening">): void;
}

function isDescendantOrSelf(node: Renderable | null, ancestor: Renderable): boolean {
	let current = node;
	while (current) {
		if (current === ancestor) return true;
		current = current.parent;
	}
	return false;
}

function isEffectivelyVisible(node: Renderable): boolean {
	let current: Renderable | null = node;
	while (current) {
		if (current.isDestroyed || !current.visible) return false;
		current = current.parent;
	}
	return true;
}

function anchorAlignment(anchor: OverlayAnchor): {
	alignItems: "flex-start" | "center" | "flex-end";
	justifyContent: "flex-start" | "center" | "flex-end";
} {
	const alignItems =
		anchor.endsWith("left") || anchor === "left-center"
			? "flex-start"
			: anchor.endsWith("right") || anchor === "right-center"
				? "flex-end"
				: "center";
	const justifyContent = anchor.startsWith("top") ? "flex-start" : anchor.startsWith("bottom") ? "flex-end" : "center";
	return { alignItems, justifyContent };
}

function validateTimeout(timeoutMs: number | undefined): void {
	if (timeoutMs === undefined) return;
	if (!Number.isFinite(timeoutMs) || timeoutMs < 0) {
		throw new RangeError("Overlay timeoutMs must be a finite non-negative number");
	}
}

class NativeOverlayHandle<Root extends Renderable> implements OverlayHandle<Root> {
	readonly id: number;
	readonly root: Root;
	readonly wrapper: BoxRenderable;
	private readonly manager: OverlayManager;
	private lifecycleState: Exclude<OverlayLifecycleState, "opening"> = "open";

	constructor(manager: OverlayManager, id: number, root: Root, wrapper: BoxRenderable) {
		this.manager = manager;
		this.id = id;
		this.root = root;
		this.wrapper = wrapper;
	}

	get state(): Exclude<OverlayLifecycleState, "opening"> {
		return this.lifecycleState;
	}

	get hidden(): boolean {
		return this.lifecycleState === "hidden";
	}

	focus(): void {
		this.manager.focus(this);
	}

	hide(): void {
		this.manager.hide(this);
	}

	show(): void {
		this.manager.show(this);
	}

	updateLayout(layout: OverlayLayout | ((viewport: OverlayViewport) => OverlayLayout)): void {
		this.manager.updateLayout(this, layout);
	}

	close(reason: OverlayCloseReason = "close"): Promise<void> {
		return this.manager.close(this, reason);
	}

	setState(state: Exclude<OverlayLifecycleState, "opening">): void {
		this.lifecycleState = state;
	}
}

export class OverlayManager {
	readonly layer: BoxRenderable;
	private readonly renderer: CliRenderer;
	private readonly entries: OverlayEntry[] = [];
	private readonly openings = new Set<OpeningEntry>();
	private readonly closedPromises = new WeakMap<OverlayHandle, Promise<void>>();
	private readonly keyHandler: (event: KeyEvent) => void;
	private readonly resizeHandler: () => void;
	private nextId = 1;
	private disposed = false;

	constructor(renderer: CliRenderer) {
		if (liveManagers.has(renderer)) throw new Error("A live OverlayManager already exists for this renderer");
		liveManagers.add(renderer);
		this.renderer = renderer;
		this.layer = new BoxRenderable(renderer, {
			id: "bone-overlay-layer",
			position: "absolute",
			top: 0,
			right: 0,
			bottom: 0,
			left: 0,
			zIndex: 10_000,
			visible: false,
		});
		renderer.root.add(this.layer);
		this.keyHandler = (event) => {
			const active = this.activeEntry();
			if (!active?.onKey || !active.onKey(event, active.handle)) return;
			event.preventDefault();
			event.stopPropagation();
		};
		this.resizeHandler = () => {
			for (const entry of this.entries) this.applyLayout(entry);
		};
		renderer.keyInput.on("keypress", this.keyHandler);
		renderer.on(CliRenderEvents.RESIZE, this.resizeHandler);
	}

	get active(): OverlayHandle | null {
		return this.activeEntry()?.handle ?? null;
	}

	get size(): number {
		return this.entries.length + this.openings.size;
	}

	open<Root extends Renderable>(
		descriptor: OverlayDescriptor<Root>,
		options: OpenOverlayOptions<Root>,
	): OverlayHandle<Root> {
		validateTimeout(options.timeoutMs);
		if (this.disposed) throw new Error("OverlayManager has been disposed");
		if (options.signal?.aborted) throw new OverlayOpenCancelledError("abort");
		return this.attach(this.nextId++, descriptor, options, options.timeoutMs);
	}

	openAsync<Root extends Renderable>(
		factory: OverlayFactory<Root>,
		options: OpenOverlayOptions<Root>,
	): Promise<OverlayHandle<Root>> {
		try {
			validateTimeout(options.timeoutMs);
			if (this.disposed) throw new Error("OverlayManager has been disposed");
		} catch (error) {
			return Promise.reject(error);
		}

		const id = this.nextId++;
		const startedAt = Date.now();
		let resolveResult: (handle: OverlayHandle<Root>) => void;
		let rejectResult: (error: unknown) => void;
		const result = new Promise<OverlayHandle<Root>>((resolve, reject) => {
			resolveResult = resolve;
			rejectResult = reject;
		});
		let timeout: ReturnType<typeof setTimeout> | undefined;
		let abortListener: (() => void) | undefined;
		let closePromise: Promise<void> | undefined;
		const cleanupOpening = () => {
			if (timeout) clearTimeout(timeout);
			if (abortListener) options.signal?.removeEventListener("abort", abortListener);
			this.openings.delete(entry);
		};
		const entry: OpeningEntry = {
			id,
			state: "opening",
			cancel: (reason) => {
				if (closePromise) return closePromise;
				if (entry.state !== "opening") return Promise.resolve();
				entry.state = "closing";
				cleanupOpening();
				closePromise = Promise.resolve()
					.then(() => options.onClose?.(reason))
					.then(
						() => {
							entry.state = "closed";
							rejectResult(new OverlayOpenCancelledError(reason));
						},
						(error: unknown) => {
							entry.state = "closed";
							rejectResult(error);
							throw error;
						},
					);
				return closePromise;
			},
		};
		this.openings.add(entry);
		abortListener = () => {
			void entry.cancel("abort").catch(() => undefined);
		};
		if (options.signal) options.signal.addEventListener("abort", abortListener, { once: true });
		if (options.timeoutMs !== undefined) {
			timeout = setTimeout(() => void entry.cancel("timeout").catch(() => undefined), options.timeoutMs);
		}
		if (options.signal?.aborted) {
			void entry.cancel("abort").catch(() => undefined);
			return result;
		}

		void Promise.resolve()
			.then(() => (entry.state === "opening" ? factory(this.renderer) : null))
			.then(
				(descriptor) => {
					if (!descriptor) {
						if (entry.state !== "opening") return;
						cleanupOpening();
						entry.state = "closed";
						const error = new Error("Overlay factory returned no descriptor");
						void Promise.resolve()
							.then(() => options.onClose?.("factory-error"))
							.then(
								() => rejectResult(error),
								() => rejectResult(error),
							);
						return;
					}
					if (entry.state !== "opening") {
						this.destroyLateDescriptor(descriptor);
						return;
					}
					cleanupOpening();
					entry.state = "closed";
					const remainingTimeout =
						options.timeoutMs === undefined
							? undefined
							: Math.max(0, options.timeoutMs - (Date.now() - startedAt));
					try {
						resolveResult(this.attach(id, descriptor, options, remainingTimeout));
					} catch (error) {
						const ownedFocus = isDescendantOrSelf(this.renderer.currentFocusedRenderable, descriptor.root);
						this.destroyLateDescriptor(descriptor);
						if (ownedFocus) this.restoreTarget(options.restoreFocus);
						void Promise.resolve()
							.then(() => options.onClose?.("factory-error"))
							.then(
								() => rejectResult(error),
								() => rejectResult(error),
							);
					}
				},
				(error: unknown) => {
					if (entry.state !== "opening") return;
					cleanupOpening();
					entry.state = "closed";
					void Promise.resolve()
						.then(() => options.onClose?.("factory-error"))
						.then(
							() => rejectResult(error),
							() => rejectResult(error),
						);
				},
			);
		return result;
	}

	focus(handle: OverlayHandle): void {
		const entry = this.findEntry(handle);
		if (!entry || entry.handle.state === "closing" || entry.handle.state === "closed") return;
		if (entry.handle.state === "hidden") this.show(handle);
		entry.focusTarget?.focus();
	}

	hide(handle: OverlayHandle): void {
		const entry = this.findEntry(handle);
		if (!entry || entry.handle.state !== "open") return;
		const current = this.renderer.currentFocusedRenderable;
		const ownedFocus = isDescendantOrSelf(current, entry.handle.wrapper);
		if (ownedFocus) current?.blur();
		entry.handle.wrapper.visible = false;
		entry.handle.setState("hidden");
		if (!this.entries.some((candidate) => candidate.handle.state === "open")) this.layer.visible = false;
		if (ownedFocus) {
			const next = this.activeEntry();
			if (next?.focusTarget) next.focusTarget.focus();
			else if (!next) this.restoreFocus(entry);
		}
		this.renderer.requestRender();
	}

	show(handle: OverlayHandle): void {
		const entry = this.findEntry(handle);
		if (!entry || entry.handle.state !== "hidden") return;
		this.layer.visible = true;
		entry.handle.wrapper.visible = true;
		entry.handle.setState("open");
		this.applyLayout(entry);
		this.renderer.requestRender();
	}

	updateLayout(handle: OverlayHandle, layout: OverlayLayout | ((viewport: OverlayViewport) => OverlayLayout)): void {
		const entry = this.findEntry(handle);
		if (!entry || entry.handle.state === "closing" || entry.handle.state === "closed") return;
		entry.layout = layout;
		this.applyLayout(entry);
		this.renderer.requestRender();
	}

	close(handle: OverlayHandle, reason: OverlayCloseReason = "close"): Promise<void> {
		const previousClose = this.closedPromises.get(handle);
		if (previousClose) return previousClose;
		const entry = this.findEntry(handle);
		if (!entry) return Promise.resolve();

		entry.handle.setState("closing");
		if (entry.timeout) clearTimeout(entry.timeout);
		if (entry.abortListener) entry.abortSignal?.removeEventListener("abort", entry.abortListener);
		entry.handle.root.off(RenderableEvents.DESTROYED, entry.contentDestroyedListener);
		const current = this.renderer.currentFocusedRenderable;
		const ownedFocus = isDescendantOrSelf(current, entry.handle.wrapper);
		const siblingFocus = current !== null && isDescendantOrSelf(current, this.layer) && !ownedFocus;
		if (ownedFocus) current?.blur();
		const index = this.entries.indexOf(entry);
		if (index !== -1) this.entries.splice(index, 1);
		entry.handle.wrapper.destroyRecursively();
		entry.handle.setState("closed");
		if (this.entries.every((candidate) => candidate.handle.state !== "open")) this.layer.visible = false;
		if (!siblingFocus && (ownedFocus || current === null)) {
			const next = this.activeEntry();
			if (next?.focusTarget) next.focusTarget.focus();
			else if (!next) this.restoreFocus(entry);
		}
		this.renderer.requestRender();

		const closePromise = Promise.resolve()
			.then(() => entry.onClose?.(reason))
			.then(() => undefined);
		this.closedPromises.set(handle, closePromise);
		return closePromise;
	}

	async dispose(): Promise<void> {
		if (this.disposed) return;
		this.disposed = true;
		liveManagers.delete(this.renderer);
		this.renderer.keyInput.off("keypress", this.keyHandler);
		this.renderer.off(CliRenderEvents.RESIZE, this.resizeHandler);
		const openingCloses = [...this.openings].map((entry) => entry.cancel("dispose"));
		const overlayCloses = [...this.entries].reverse().map((entry) => this.close(entry.handle, "dispose"));
		await Promise.all([...openingCloses, ...overlayCloses]);
		if (!this.layer.isDestroyed) this.layer.destroyRecursively();
		this.renderer.requestRender();
	}

	private attach<Root extends Renderable>(
		id: number,
		descriptor: OverlayDescriptor<Root>,
		options: OpenOverlayOptions<Root>,
		timeoutMs: number | undefined,
	): OverlayHandle<Root> {
		this.validateDescriptor(descriptor);
		const wrapper = new BoxRenderable(this.renderer, {
			id: `bone-overlay-${id}`,
			position: "absolute",
			top: 0,
			right: 0,
			bottom: 0,
			left: 0,
			// A non-focusable overlay still needs a native focus owner so
			// ordinary text cannot fall through to the composer while modal.
			focusable: descriptor.focusTarget === undefined || descriptor.focusTarget === null,
		});
		const handle = new NativeOverlayHandle(this, id, descriptor.root, wrapper);
		const contentDestroyedListener = () => {
			void handle.close("content-destroyed");
		};
		const abortListener = options.signal
			? () => {
					void handle.close("abort").catch(() => undefined);
				}
			: undefined;
		const entry: OverlayEntry = {
			handle,
			restoreFocus: options.restoreFocus,
			focusTarget: descriptor.focusTarget ?? wrapper,
			onKey: options.onKey ? (event) => options.onKey?.(event, handle) ?? false : undefined,
			onClose: options.onClose,
			abortSignal: options.signal,
			abortListener,
			contentDestroyedListener,
			layout: options.layout ?? {},
			timeout: undefined,
		};

		wrapper.add(descriptor.root);
		this.applyLayout(entry);
		this.layer.visible = true;
		this.layer.add(wrapper);
		this.entries.push(entry);
		descriptor.root.once(RenderableEvents.DESTROYED, contentDestroyedListener);
		if (abortListener) options.signal?.addEventListener("abort", abortListener, { once: true });
		if (timeoutMs !== undefined) {
			entry.timeout = setTimeout(() => void handle.close("timeout").catch(() => undefined), timeoutMs);
		}
		entry.focusTarget?.focus();
		this.renderer.requestRender();
		return handle;
	}

	private validateDescriptor(descriptor: OverlayDescriptor): void {
		const root = descriptor.root;
		if (root.isDestroyed) throw new Error("Cannot open a destroyed overlay root");
		if (root.parent) throw new Error("Overlay root must be detached before open()");
		if (descriptor.focusTarget && !isDescendantOrSelf(descriptor.focusTarget, root)) {
			throw new Error("Overlay focusTarget must belong to the overlay root subtree");
		}
		if (descriptor.focusTarget && (!descriptor.focusTarget.focusable || descriptor.focusTarget.isDestroyed)) {
			throw new Error("Overlay focusTarget must be a live focusable renderable");
		}
		if (isDescendantOrSelf(this.renderer.currentFocusedRenderable, root)) {
			throw new Error("Overlay content must not focus a control before native tree attachment");
		}
	}

	private destroyLateDescriptor(descriptor: OverlayDescriptor): void {
		if (!descriptor.root.isDestroyed) descriptor.root.destroyRecursively();
	}

	private activeEntry(): OverlayEntry | undefined {
		for (let index = this.entries.length - 1; index >= 0; index--) {
			const entry = this.entries[index];
			if (entry?.handle.state === "open") return entry;
		}
		return undefined;
	}

	private findEntry(handle: OverlayHandle): OverlayEntry | undefined {
		return this.entries.find((entry) => entry.handle === handle);
	}

	private applyLayout(entry: OverlayEntry): void {
		const layout =
			typeof entry.layout === "function"
				? entry.layout({ width: this.renderer.width, height: this.renderer.height })
				: entry.layout;
		const alignment = anchorAlignment(layout.anchor ?? "center");
		entry.handle.wrapper.alignItems = alignment.alignItems;
		entry.handle.wrapper.justifyContent = alignment.justifyContent;
		entry.handle.wrapper.padding = layout.margin ?? 0;
		entry.handle.wrapper.zIndex = layout.zIndex ?? entry.handle.id;
		if (layout.backdropColor !== undefined) entry.handle.wrapper.backgroundColor = layout.backdropColor;
		if (layout.width !== undefined) entry.handle.root.width = layout.width;
		if (layout.height !== undefined) entry.handle.root.height = layout.height;
		if (layout.minWidth !== undefined && layout.minWidth !== "auto") entry.handle.root.minWidth = layout.minWidth;
		if (layout.minHeight !== undefined && layout.minHeight !== "auto") entry.handle.root.minHeight = layout.minHeight;
		if (layout.maxWidth !== undefined && layout.maxWidth !== "auto") entry.handle.root.maxWidth = layout.maxWidth;
		if (layout.maxHeight !== undefined && layout.maxHeight !== "auto") entry.handle.root.maxHeight = layout.maxHeight;
	}

	private restoreFocus(entry: OverlayEntry): void {
		this.restoreTarget(entry.restoreFocus);
	}

	private restoreTarget(target: Renderable | null): void {
		if (!target || !isEffectivelyVisible(target)) return;
		if (this.activeEntry() && !isDescendantOrSelf(target, this.layer)) return;
		target.focus();
	}
}
