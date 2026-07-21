export type ScrollDirection = "up" | "down";

export type KineticScrollStep = {
	direction: ScrollDirection;
	lineCount: number;
};

const INITIAL_INPUT_IMPULSE = 18;
const CONTINUOUS_INPUT_IMPULSE = 40;
const CONTINUOUS_INPUT_WINDOW_MS = 70;
const DECAY_TIME_CONSTANT_MS = 110;
const MAX_VELOCITY_LINES_PER_SECOND = 600;
const MAX_FRAME_DURATION_MS = 32;
const MAX_LINES_PER_FRAME = 8;
const STOP_VELOCITY_LINES_PER_SECOND = 4;

function directionSign(direction: ScrollDirection): number {
	return direction === "up" ? 1 : -1;
}

function directionForSign(sign: number): ScrollDirection {
	return sign >= 0 ? "up" : "down";
}

/**
 * Converts discrete terminal wheel events into a short, velocity-based scroll
 * gesture. Terminal mouse reporting has no pixel delta or native momentum, so
 * this controller estimates it from input density and emits bounded line steps
 * suitable for a row-based TUI.
 */
export class KineticScrollController {
	private velocity = 0;
	private fractionalDistance = 0;
	private lastInputAt: number | undefined;
	private lastFrameAt: number | undefined;

	get active(): boolean {
		return Math.abs(this.velocity) >= STOP_VELOCITY_LINES_PER_SECOND;
	}

	/**
	 * Records one wheel event and returns the immediate line to move for direct
	 * feedback. Subsequent movement is emitted from advance().
	 */
	receive(direction: ScrollDirection, now: number): KineticScrollStep {
		const sign = directionSign(direction);
		const elapsedSinceInput = this.lastInputAt === undefined ? Number.POSITIVE_INFINITY : now - this.lastInputAt;
		const wasMovingOppositeDirection = this.velocity !== 0 && Math.sign(this.velocity) !== sign;
		if (wasMovingOppositeDirection) {
			this.velocity = 0;
			this.fractionalDistance = 0;
		}

		const impulse =
			elapsedSinceInput <= CONTINUOUS_INPUT_WINDOW_MS ? CONTINUOUS_INPUT_IMPULSE : INITIAL_INPUT_IMPULSE;
		this.velocity = Math.max(
			-MAX_VELOCITY_LINES_PER_SECOND,
			Math.min(MAX_VELOCITY_LINES_PER_SECOND, this.velocity + sign * impulse),
		);
		this.lastInputAt = now;
		this.lastFrameAt ??= now;
		return { direction, lineCount: 1 };
	}

	/** Advance the kinetic gesture to a specific time and emit at most one bounded line step. */
	advance(now: number): KineticScrollStep | undefined {
		if (!this.active || this.lastFrameAt === undefined) return undefined;

		const elapsed = Math.max(0, Math.min(MAX_FRAME_DURATION_MS, now - this.lastFrameAt));
		this.lastFrameAt = now;
		if (elapsed === 0) return undefined;

		this.fractionalDistance += (this.velocity * elapsed) / 1000;
		const sign = Math.sign(this.fractionalDistance);
		const lineCount = Math.min(MAX_LINES_PER_FRAME, Math.floor(Math.abs(this.fractionalDistance)));
		this.velocity *= Math.exp(-elapsed / DECAY_TIME_CONSTANT_MS);

		if (lineCount > 0) {
			this.fractionalDistance -= sign * lineCount;
			return { direction: directionForSign(sign), lineCount };
		}

		if (!this.active) this.fractionalDistance = 0;
		return undefined;
	}

	cancel(): void {
		this.velocity = 0;
		this.fractionalDistance = 0;
		this.lastInputAt = undefined;
		this.lastFrameAt = undefined;
	}
}
