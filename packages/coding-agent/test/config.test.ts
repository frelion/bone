import { describe, expect, test } from "vitest";
import { detectInstallMethod, getSelfUpdateCommand, getUpdateInstruction, isBunRuntime } from "../src/config.ts";

describe("Bun installation contract", () => {
	test("runs only under Bun", () => {
		expect(isBunRuntime).toBe(true);
		expect(detectInstallMethod()).toBe("bun");
	});

	test("does not expose a Node package-manager update command", () => {
		const command = getSelfUpdateCommand("@frelion/bone-coding-agent");
		if (command) {
			expect(command.command).toBe("bun");
			expect(command.args[0]).toBe("install");
			expect(command.args).not.toContain("npm");
			expect(command.args).not.toContain("pnpm");
			expect(command.args).not.toContain("yarn");
		}
	});

	test("uses Bun for update instructions", () => {
		expect(getUpdateInstruction("@frelion/bone-coding-agent")).toBe(
			"Run: bun install -g --ignore-scripts --minimum-release-age=0 @frelion/bone-coding-agent",
		);
	});
});
