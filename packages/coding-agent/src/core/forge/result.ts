import type { ForgePage } from "./contracts.ts";
import { ForgeError } from "./errors.ts";

export type ForgeQueryResource = "issue" | "milestone" | "change" | "wiki" | "pipeline" | "job" | "release";

export const DEFAULT_FORGE_QUERY_LIMIT = 10;
export const MAX_FORGE_QUERY_LIMIT = 50;
export const MAX_FORGE_TOOL_RESULT_BYTES = 64 * 1024;
export const MAX_FORGE_BATCH_IDS = 5;

const MAX_LIST_TEXT = 512;
const MAX_URL_TEXT = 2_048;
export const MAX_FORGE_DETAIL_BODY_BYTES = 16 * 1024;
export const MAX_FORGE_BATCH_BODY_BYTES = 8 * 1024;
export const MAX_FORGE_BODY_PREVIEW_BYTES = 384;
const MAX_COLLECTION_ITEMS = 20;

function object(value: unknown): Record<string, unknown> | undefined {
	return typeof value === "object" && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: undefined;
}

function text(value: unknown, maximum = MAX_LIST_TEXT): string | undefined {
	if (typeof value !== "string") return undefined;
	return value.length <= maximum ? value : `${value.slice(0, maximum)}...`;
}

function number(value: unknown): number | undefined {
	return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function boolean(value: unknown): boolean | undefined {
	return typeof value === "boolean" ? value : undefined;
}

function nestedText(value: unknown, keys: readonly string[]): string | undefined {
	const entry = object(value);
	if (!entry) return undefined;
	for (const key of keys) {
		const result = text(entry[key]);
		if (result !== undefined) return result;
	}
	return undefined;
}

function names(value: unknown): string[] | undefined {
	if (!Array.isArray(value)) return undefined;
	const result = value
		.slice(0, MAX_COLLECTION_ITEMS)
		.flatMap((entry) => {
			if (typeof entry === "string") return [text(entry, 128)];
			const name = nestedText(entry, ["name", "username", "login"]);
			return name ? [name] : [];
		})
		.filter((entry): entry is string => entry !== undefined);
	return result.length > 0 ? result : undefined;
}

function compactRecord(value: Record<string, unknown>): Record<string, unknown> {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function truncateUtf8(
	value: string,
	maximumBytes: number,
): { value: string; originalBytes: number; truncated: boolean } {
	let bytes = 0;
	let bounded = "";
	for (const character of value) {
		const characterBytes = Buffer.byteLength(character, "utf8");
		if (bytes + characterBytes > maximumBytes) break;
		bounded += character;
		bytes += characterBytes;
	}
	const originalBytes = Buffer.byteLength(value, "utf8");
	return { value: bounded, originalBytes, truncated: originalBytes > bytes };
}

function projectedBody(
	value: Record<string, unknown>,
	mode: "list" | "detail",
	maximumBytes: number,
): Record<string, unknown> {
	const body = [value.description, value.body, value.content].find((entry) => typeof entry === "string");
	if (typeof body !== "string") return {};
	const bounded = truncateUtf8(body, maximumBytes);
	if (mode === "list") {
		return {
			bodyPreview: bounded.value,
			bodyPreviewTruncated: bounded.truncated || undefined,
		};
	}
	return {
		body: bounded.value,
		bodyTruncated: bounded.truncated || undefined,
		bodyOriginalBytes: bounded.truncated ? bounded.originalBytes : undefined,
	};
}

export function projectForgeResource(
	resource: ForgeQueryResource,
	value: unknown,
	mode: "list" | "detail",
	detailBodyBytes = MAX_FORGE_DETAIL_BODY_BYTES,
): Record<string, unknown> {
	const entry = object(value) ?? {};
	const pipeline = object(entry.pipeline);
	const projected = compactRecord({
		id: number(entry.id) ?? text(entry.id, 128),
		number: number(entry.iid) ?? number(entry.number),
		key: text(entry.slug, 256) ?? text(entry.tag_name, 256),
		title: text(entry.title) ?? text(entry.name),
		state: text(entry.state, 128) ?? text(entry.status, 128),
		conclusion: text(entry.conclusion, 128),
		author: nestedText(entry.author, ["username", "name"]) ?? nestedText(entry.user, ["login", "name"]),
		assignees: names(entry.assignees),
		labels: names(entry.labels),
		milestone: nestedText(entry.milestone, ["title"]),
		createdAt: text(entry.created_at, 128),
		updatedAt: text(entry.updated_at, 128),
		dueDate: text(entry.due_date, 128),
		webUrl: text(entry.web_url, MAX_URL_TEXT) ?? text(entry.html_url, MAX_URL_TEXT),
		ref: text(entry.ref, 256),
		sha: text(entry.sha, 128) ?? text(entry.head_sha, 128),
		stage: text(entry.stage, 256),
		pipelineId: number(pipeline?.id),
		sourceBranch: text(entry.source_branch, 256) ?? nestedText(entry.head, ["ref"]),
		targetBranch: text(entry.target_branch, 256) ?? nestedText(entry.base, ["ref"]),
		draft: boolean(entry.draft) ?? boolean(entry.work_in_progress),
		mergeStatus: text(entry.merge_status, 128) ?? text(entry.mergeable_state, 128),
		tagName: text(entry.tag_name, 256),
		releasedAt: text(entry.released_at, 128) ?? text(entry.published_at, 128),
		format: resource === "wiki" ? text(entry.format, 64) : undefined,
	});
	if (projected.id === undefined && projected.number === undefined && projected.key === undefined) {
		throw new ForgeError("invalid_remote_response", `Forge ${resource} response has no stable identifier`);
	}
	return {
		...projected,
		...projectedBody(entry, mode, mode === "list" ? MAX_FORGE_BODY_PREVIEW_BYTES : detailBodyBytes),
	};
}

export function projectForgePage(resource: ForgeQueryResource, page: ForgePage<unknown>): Record<string, unknown> {
	const items = page.items.map((item) => projectForgeResource(resource, item, "list"));
	const originalCount = items.length;
	const result = () =>
		compactRecord({
			resource,
			mode: "list",
			items,
			returned: items.length,
			hasMore: page.hasMore || items.length < originalCount,
			nextCursor: page.nextCursor,
			truncated: items.length < originalCount || undefined,
			omittedItems: items.length < originalCount ? originalCount - items.length : undefined,
			truncationReason: items.length < originalCount ? "output_budget" : undefined,
		});
	while (
		items.length > 0 &&
		Buffer.byteLength(JSON.stringify(result()), "utf8") > MAX_FORGE_TOOL_RESULT_BYTES - 1_024
	) {
		items.pop();
	}
	return result();
}

export function projectForgeDetail(resource: ForgeQueryResource, value: unknown): Record<string, unknown> {
	return { resource, mode: "detail", item: projectForgeResource(resource, value, "detail") };
}

export function projectForgeBatch(resource: ForgeQueryResource, values: readonly unknown[]): Record<string, unknown> {
	const items = values.map((value) => projectForgeResource(resource, value, "detail", MAX_FORGE_BATCH_BODY_BYTES));
	const originalCount = items.length;
	const result = () =>
		compactRecord({
			resource,
			mode: "batch",
			items,
			returned: items.length,
			truncated: items.length < originalCount || undefined,
			omittedItems: items.length < originalCount ? originalCount - items.length : undefined,
			truncationReason: items.length < originalCount ? "output_budget" : undefined,
		});
	while (
		items.length > 0 &&
		Buffer.byteLength(JSON.stringify(result()), "utf8") > MAX_FORGE_TOOL_RESULT_BYTES - 1_024
	) {
		items.pop();
	}
	return result();
}

export function boundedForgeToolResult(value: unknown): { value: unknown; text: string } {
	const serialized = typeof value === "string" ? value : (JSON.stringify(value, null, 2) ?? "null");
	const originalBytes = Buffer.byteLength(serialized, "utf8");
	if (originalBytes <= MAX_FORGE_TOOL_RESULT_BYTES) return { value, text: serialized };
	const bounded = {
		truncated: true,
		reason: "output_budget",
		originalBytes,
		maximumBytes: MAX_FORGE_TOOL_RESULT_BYTES,
		message:
			"Forge result omitted because it exceeded the tool output budget. Narrow the query or request one item by id.",
	};
	return { value: bounded, text: JSON.stringify(bounded, null, 2) };
}
