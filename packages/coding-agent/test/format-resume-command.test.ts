import { afterEach, describe, expect, it } from "vitest";
import { APP_NAME } from "../src/config.ts";
import { formatWorkspaceReturnHint } from "../src/modes/interactive/interactive-mode.ts";

const originalStdoutIsTTY = Object.getOwnPropertyDescriptor(process.stdout, "isTTY");

afterEach(() => {
	if (originalStdoutIsTTY) {
		Object.defineProperty(process.stdout, "isTTY", originalStdoutIsTTY);
	} else {
		Reflect.deleteProperty(process.stdout, "isTTY");
	}
});

function setStdoutIsTTY(value: boolean): void {
	Object.defineProperty(process.stdout, "isTTY", { configurable: true, value });
}

describe("formatWorkspaceReturnHint", () => {
	it("guides interactive users back to their workspace without exposing storage IDs", () => {
		setStdoutIsTTY(true);

		expect(formatWorkspaceReturnHint()).toBe(
			`Reopen ${APP_NAME} in this workspace to choose a conversation from Side.`,
		);
	});

	it("does not print a hint outside an interactive terminal", () => {
		setStdoutIsTTY(false);

		expect(formatWorkspaceReturnHint()).toBeUndefined();
	});
});
