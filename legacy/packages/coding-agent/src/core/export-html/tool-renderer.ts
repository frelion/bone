/** Pure-data HTML rendering for tool calls and results. */

import { stripAnsi } from "../../utils/ansi.ts";
import type { ToolDefinition } from "../extensions/types.ts";
import { ansiLinesToHtml } from "./ansi-to-html.ts";

export interface ToolHtmlRendererDeps {
	getToolDefinition: (name: string) => ToolDefinition | undefined;
	theme: unknown;
	cwd: string;
	width?: number;
}

export interface ToolHtmlRenderer {
	renderCall(toolCallId: string, toolName: string, args: unknown): string | undefined;
	renderResult(
		toolCallId: string,
		toolName: string,
		result: Array<{ type: string; text?: string; data?: string; mimeType?: string }>,
		details: unknown,
		isError: boolean,
	): { collapsed?: string; expanded?: string } | undefined;
}

function trimBlankLines(lines: string[]): string[] {
	let start = 0;
	let end = lines.length;
	while (start < end && stripAnsi(lines[start] ?? "").trim().length === 0) start++;
	while (end > start && stripAnsi(lines[end - 1] ?? "").trim().length === 0) end--;
	return lines.slice(start, end);
}

function callLines(toolName: string, args: unknown): string[] {
	let serialized: string;
	try {
		serialized = JSON.stringify(args, null, 2) ?? "";
	} catch {
		serialized = String(args ?? "");
	}
	return serialized && serialized !== "{}" ? [`${toolName}`, ...serialized.split("\n")] : [toolName];
}

function resultLines(result: Array<{ type: string; text?: string; mimeType?: string }>): string[] {
	const lines: string[] = [];
	for (const part of result) {
		if (part.type === "text") lines.push(...(part.text ?? "").replace(/\r/g, "").split("\n"));
		else if (part.type === "image") lines.push(`[image: ${part.mimeType ?? "image/unknown"}]`);
	}
	return trimBlankLines(lines);
}

export function createToolHtmlRenderer(deps: ToolHtmlRendererDeps): ToolHtmlRenderer {
	return {
		renderCall(_toolCallId, toolName, args) {
			const definition = deps.getToolDefinition(toolName);
			if (!definition?.renderV2?.renderCall) return undefined;
			return ansiLinesToHtml(callLines(toolName, args));
		},

		renderResult(_toolCallId, toolName, result) {
			const definition = deps.getToolDefinition(toolName);
			if (!definition?.renderV2?.renderResult) return undefined;
			const lines = resultLines(result);
			const expanded = ansiLinesToHtml(lines);
			const collapsedLines = lines.length > 20 ? lines.slice(-20) : lines;
			const collapsed = ansiLinesToHtml(collapsedLines);
			return {
				...(collapsed !== expanded ? { collapsed } : {}),
				expanded,
			};
		},
	};
}
