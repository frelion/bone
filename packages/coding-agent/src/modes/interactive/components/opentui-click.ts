import type { MouseEvent, Renderable } from "@opentui/core";

type OpenTUIClickAction = () => void;

/** Coordinates completed primary-button clicks across nested OpenTUI renderables. */
export class OpenTUIClickCoordinator {
	private readonly actions = new WeakMap<Renderable, OpenTUIClickAction>();
	private pressedSurface: Renderable | undefined;

	register(surface: Renderable, action: OpenTUIClickAction): void {
		this.actions.set(surface, action);
	}

	handle(event: MouseEvent): boolean {
		if (event.type === "down") {
			this.pressedSurface = undefined;
			if (event.button !== 0) return false;
			const surface = this.findSurface(event.target);
			if (!surface) return false;
			this.pressedSurface = surface;
			event.preventDefault();
			event.stopPropagation();
			return true;
		}

		if (event.type === "drag" || event.type === "drag-end") {
			this.pressedSurface = undefined;
			return false;
		}

		if (event.type !== "up") return false;
		const pressedSurface = this.pressedSurface;
		this.pressedSurface = undefined;
		if (event.button !== 0 || event.isDragging || !pressedSurface) return false;
		const releasedSurface = this.findSurface(event.target);
		if (releasedSurface !== pressedSurface) return false;
		const action = this.actions.get(pressedSurface);
		if (!action) return false;
		event.preventDefault();
		event.stopPropagation();
		action();
		return true;
	}

	reset(): void {
		this.pressedSurface = undefined;
	}

	private findSurface(target: Renderable | null): Renderable | undefined {
		let current = target;
		while (current) {
			if (this.actions.has(current)) return current;
			current = current.parent;
		}
		return undefined;
	}
}
