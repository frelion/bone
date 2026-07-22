import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { parseOsc11BackgroundColor, parseTerminalColorSchemeReport } from "../src/terminal-colors.ts";

describe("terminal color report parsing", () => {
	test("parses OSC 11 rgb and hex responses", () => {
		assert.deepEqual(parseOsc11BackgroundColor("\x1b]11;rgb:0000/8000/ffff\x07"), { r: 0, g: 128, b: 255 });
		assert.deepEqual(parseOsc11BackgroundColor("\x1b]11;#ffffff\x1b\\"), { r: 255, g: 255, b: 255 });
	});

	test("rejects malformed or unrelated reports", () => {
		assert.equal(parseOsc11BackgroundColor("\x1b]10;#ffffff\x07"), undefined);
		assert.equal(parseOsc11BackgroundColor("prefix\x1b]11;#ffffff\x07"), undefined);
	});

	test("parses terminal color scheme reports", () => {
		assert.equal(parseTerminalColorSchemeReport("\x1b[?997;1n"), "dark");
		assert.equal(parseTerminalColorSchemeReport("\x1b[?997;2n"), "light");
		assert.equal(parseTerminalColorSchemeReport("invalid"), undefined);
	});
});
