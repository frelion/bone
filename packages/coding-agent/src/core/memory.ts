import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { AgentMessage } from "@frelion/bone-agent-core";
import * as lancedb from "@lancedb/lancedb";
import lockfile from "proper-lockfile";
import {
	getLocalEmbeddingAvailability,
	type LocalEmbeddingEngine,
	type LocalEmbeddingEngineDiagnostics,
	type LocalEmbeddingStatus,
	LocalEmbeddingWorker,
} from "./local-embedding.ts";
import { isMessageWithContent, parseSessionEntryLine, type SessionEntry, type SessionInfo } from "./session-manager.ts";
import { isCodeLikeSearchQuery, normalizeSearchPreview, normalizeSearchTerms } from "./session-search-normalizer.ts";

// v3 keeps mutable conversation titles in one dedicated document. Previous
// derived stores duplicated title terms on every exchange, so rebuild instead
// of carrying stale title tokens forward.
const MEMORY_VERSION = "v3";
const VECTOR_DIMENSIONS = 384;
const ITEMS_TABLE = "memory_items";
const STATE_TABLE = "memory_state";
const ANN_INDEX_THRESHOLD = 10_000;
const VECTOR_INDEX_NAME = "embedding_hnsw_sq";
const EMBEDDING_LOCK_FILE = ".embedding.lock";
const POLL_BATCH_SIZE = 8;
const INITIAL_POLL_DELAY_MS = 500;
const MAX_POLL_DELAY_MS = 5_000;

export type MemoryKind = "conversation-exchange" | "conversation-title" | "file-reference" | "command-reference";
export type MemoryContentStatus = "pending" | "complete";
export type MemoryEmbeddingState = "pending" | "ready" | "failed";
export type MemoryEvidenceKind = "title" | "user" | "assistant" | "file" | "command" | "semantic";

export interface MemorySearchResult {
	sessionPath: string;
	score: number;
	evidence: {
		kind: MemoryEvidenceKind;
		label: "Title" | "You" | "Bone" | "File" | "Command" | "Related discussion";
		snippet: string;
	};
}

export interface MemoryRuntimeStatus {
	phase: "preparing" | "ready" | "unavailable";
	message?: string;
}

export type MemoryStoreState = "preparing" | "ready" | "unavailable";
export type MemoryEmbeddingWorkerState =
	| "not-started"
	| "starting"
	| "active"
	| "idle"
	| "unavailable"
	| "another-process";

export type MemoryIndexingState =
	| "starting"
	| "queued"
	| "embedding"
	| "up-to-date"
	| "unavailable"
	| "another-process";

export interface MemoryIndexingDiagnostics {
	state: MemoryIndexingState;
	pending: number;
	active: number;
}
export type MemoryVectorIndexMode = "flat" | "hnsw-sq";

/**
 * A read-only health snapshot for the workspace-local memory store.
 *
 * Gathering this data never reconciles JSONL, creates an index, starts an
 * embedding worker, or prepares/downloads the local embedding model.
 */
export interface MemoryRuntimeDiagnostics {
	store: MemoryStoreState;
	conversations: number;
	exchanges: number;
	embeddings: Record<MemoryEmbeddingState, number>;
	worker: MemoryEmbeddingWorkerState;
	engine: LocalEmbeddingEngineDiagnostics;
	indexing: MemoryIndexingDiagnostics;
	vectorIndex: MemoryVectorIndexMode;
	semantic: MemoryRuntimeStatus;
}

export interface MemoryItem {
	id: string;
	sessionPath: string;
	sessionId: string;
	kind: MemoryKind;
	sourceEntryId: string;
	titleText: string;
	bodyText: string;
	referenceText: string;
	displayText: string;
	semanticText: string;
	contentStatus: MemoryContentStatus;
	sourceHash: string;
	embedding: number[];
	embeddingState: MemoryEmbeddingState;
	embeddingSourceHash: string;
	createdAt: number;
	updatedAt: number;
}

interface LanceMemoryRow {
	id?: unknown;
	sessionPath?: unknown;
	sessionId?: unknown;
	kind?: unknown;
	sourceEntryId?: unknown;
	titleText?: unknown;
	bodyText?: unknown;
	referenceText?: unknown;
	displayText?: unknown;
	semanticText?: unknown;
	contentStatus?: unknown;
	sourceHash?: unknown;
	embedding?: unknown;
	embeddingState?: unknown;
	embeddingSourceHash?: unknown;
	createdAt?: unknown;
	updatedAt?: unknown;
	_score?: unknown;
	_distance?: unknown;
	_relevance_score?: unknown;
}

interface MemoryStateRow {
	key: string;
	sessionPath: string;
	fingerprint: string;
	updatedAt: number;
}

interface PendingEmbedding {
	id: string;
	sourceHash: string;
	semanticText: string;
}

interface MemoryRepositoryDiagnostics {
	conversations: number;
	exchanges: number;
	embeddings: Record<MemoryEmbeddingState, number>;
	vectorIndex: MemoryVectorIndexMode;
}

interface ExtractedReference {
	kind: "file-reference" | "command-reference";
	text: string;
}

interface PendingExchange {
	id: string;
	sourceEntryId: string;
	userText: string;
}

function stableHash(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function quotePredicate(value: string): string {
	return `'${value.replaceAll("'", "''")}'`;
}

function asString(value: unknown): string | undefined {
	return typeof value === "string" ? value : undefined;
}

function asNumber(value: unknown): number | undefined {
	if (typeof value === "number" && Number.isFinite(value)) return value;
	if (typeof value === "bigint") {
		const number = Number(value);
		return Number.isSafeInteger(number) ? number : undefined;
	}
	return undefined;
}

function asEmbedding(value: unknown): number[] | undefined {
	if (Array.isArray(value) && value.every((entry) => typeof entry === "number" && Number.isFinite(entry)))
		return value;
	if (value instanceof Float32Array || value instanceof Float64Array) return [...value];
	if (
		value &&
		typeof value === "object" &&
		"toArray" in value &&
		typeof (value as { toArray?: unknown }).toArray === "function"
	) {
		return asEmbedding((value as { toArray: () => unknown }).toArray());
	}
	return undefined;
}

function isVerifiedEmbedding(vector: ArrayLike<number>): boolean {
	if (vector.length !== VECTOR_DIMENSIONS) return false;
	let normSquared = 0;
	for (let index = 0; index < vector.length; index++) {
		const value = vector[index];
		if (!Number.isFinite(value)) return false;
		normSquared += value * value;
	}
	return Number.isFinite(normSquared) && normSquared > 1e-12;
}

function asMemoryKind(value: unknown): MemoryKind | undefined {
	return value === "conversation-exchange" ||
		value === "conversation-title" ||
		value === "file-reference" ||
		value === "command-reference"
		? value
		: undefined;
}

function asEmbeddingState(value: unknown): MemoryEmbeddingState | undefined {
	return value === "pending" || value === "ready" || value === "failed" ? value : undefined;
}

function textFromMessage(message: AgentMessage): string {
	if (!isMessageWithContent(message)) return "";
	if (typeof message.content === "string") return message.content;
	return message.content
		.filter((part): part is { type: "text"; text: string } => part.type === "text")
		.map((part) => part.text)
		.join("\n");
}

function isFinalAssistantMessage(message: AgentMessage): boolean {
	if (!isMessageWithContent(message) || message.role !== "assistant") return false;
	if (message.stopReason !== "stop") return false;
	const content: unknown = message.content;
	if (typeof content === "string") return content.trim().length > 0;
	if (!Array.isArray(content)) return false;
	return (
		!content.some(
			(part) => typeof part === "object" && part !== null && "type" in part && part.type === "toolCall",
		) && textFromMessage(message).trim().length > 0
	);
}

function extractReferences(text: string): ExtractedReference[] {
	const files = new Set<string>();
	const commands = new Set<string>();
	for (const match of text.matchAll(
		/(?:[A-Za-z0-9_.-]+[\\/])+[A-Za-z0-9_.-]+|\b[A-Za-z0-9_.-]+\.(?:ts|tsx|js|jsx|json|md|yaml|yml|toml|css|sh)\b/g,
	)) {
		files.add(match[0]);
	}
	for (const match of text.matchAll(/\b(?:npm|pnpm|bun|git|bone)\s+[A-Za-z0-9:_./-]+/g)) {
		commands.add(match[0]);
	}
	return [
		...Array.from(files, (value) => ({ kind: "file-reference" as const, text: value })),
		...Array.from(commands, (value) => ({ kind: "command-reference" as const, text: value })),
	];
}

function labelForKind(kind: MemoryEvidenceKind): MemorySearchResult["evidence"]["label"] {
	switch (kind) {
		case "title":
			return "Title";
		case "user":
			return "You";
		case "assistant":
			return "Bone";
		case "file":
			return "File";
		case "command":
			return "Command";
		case "semantic":
			return "Related discussion";
	}
}

function scoreForKind(kind: MemoryEvidenceKind): number {
	switch (kind) {
		case "title":
			return 8;
		case "file":
		case "command":
			return 6;
		case "user":
			return 4;
		case "assistant":
			return 3;
		case "semantic":
			return 2;
	}
}

function evidenceKind(kind: MemoryKind, semantic: boolean): MemoryEvidenceKind {
	if (semantic) return "semantic";
	switch (kind) {
		case "conversation-title":
			return "title";
		case "file-reference":
			return "file";
		case "command-reference":
			return "command";
		case "conversation-exchange":
			return "assistant";
	}
}

function memoryItemFromRow(row: LanceMemoryRow): MemoryItem | undefined {
	const id = asString(row.id);
	const sessionPath = asString(row.sessionPath);
	const sessionId = asString(row.sessionId);
	const kind = asMemoryKind(row.kind);
	const sourceEntryId = asString(row.sourceEntryId);
	const titleText = asString(row.titleText);
	const bodyText = asString(row.bodyText);
	const referenceText = asString(row.referenceText);
	const displayText = asString(row.displayText);
	const semanticText = asString(row.semanticText);
	const contentStatus =
		row.contentStatus === "complete" ? "complete" : row.contentStatus === "pending" ? "pending" : undefined;
	const sourceHash = asString(row.sourceHash);
	const embedding = asEmbedding(row.embedding);
	const embeddingState = asEmbeddingState(row.embeddingState);
	const embeddingSourceHash = asString(row.embeddingSourceHash);
	const createdAt = asNumber(row.createdAt);
	const updatedAt = asNumber(row.updatedAt);
	if (
		!id ||
		!sessionPath ||
		!sessionId ||
		!kind ||
		!sourceEntryId ||
		titleText === undefined ||
		bodyText === undefined ||
		referenceText === undefined ||
		displayText === undefined ||
		semanticText === undefined ||
		!contentStatus ||
		!sourceHash ||
		!embedding ||
		!embeddingState ||
		embeddingSourceHash === undefined ||
		createdAt === undefined ||
		updatedAt === undefined
	) {
		return undefined;
	}
	return {
		id,
		sessionPath,
		sessionId,
		kind,
		sourceEntryId,
		titleText,
		bodyText,
		referenceText,
		displayText,
		semanticText,
		contentStatus,
		sourceHash,
		embedding,
		embeddingState,
		embeddingSourceHash,
		createdAt,
		updatedAt,
	};
}

function createExchangeItem(input: {
	sessionPath: string;
	sessionId: string;
	sourceEntryId: string;
	userText: string;
	assistantText?: string;
	createdAt: number;
}): MemoryItem {
	const assistantText = input.assistantText?.trim();
	const body = assistantText
		? `User task: ${input.userText}\nFinal result: ${assistantText}`
		: `User task: ${input.userText}`;
	const sourceHash = stableHash(body);
	return {
		id: stableHash(`${input.sessionPath}:exchange:${input.sourceEntryId}`),
		sessionPath: input.sessionPath,
		sessionId: input.sessionId,
		kind: "conversation-exchange",
		sourceEntryId: input.sourceEntryId,
		// Conversation titles are indexed only by their dedicated, mutable title
		// item. Repeating a title on every exchange made renamed titles accumulate
		// in the FTS corpus and could surface an obsolete name as message evidence.
		titleText: "",
		bodyText: normalizeSearchTerms(body),
		referenceText: normalizeSearchTerms(
			extractReferences(body)
				.map((reference) => reference.text)
				.join(" "),
		),
		displayText: assistantText || input.userText,
		semanticText: body,
		contentStatus: assistantText ? "complete" : "pending",
		sourceHash,
		embedding: new Array(VECTOR_DIMENSIONS).fill(0),
		embeddingState: "pending",
		embeddingSourceHash: "",
		createdAt: input.createdAt,
		updatedAt: Date.now(),
	};
}

function createTitleItem(input: {
	sessionPath: string;
	sessionId: string;
	title: string;
	updatedAt: number;
}): MemoryItem | undefined {
	const title = input.title.trim();
	if (!title) return undefined;
	return {
		id: stableHash(`${input.sessionPath}:title`),
		sessionPath: input.sessionPath,
		sessionId: input.sessionId,
		kind: "conversation-title",
		sourceEntryId: "session-title",
		titleText: normalizeSearchTerms(title),
		bodyText: "",
		referenceText: "",
		displayText: title,
		semanticText: "",
		contentStatus: "complete",
		sourceHash: stableHash(title),
		embedding: new Array(VECTOR_DIMENSIONS).fill(0),
		embeddingState: "failed",
		embeddingSourceHash: "",
		createdAt: input.updatedAt,
		updatedAt: input.updatedAt,
	};
}

function createReferenceItems(exchange: MemoryItem): MemoryItem[] {
	return extractReferences(exchange.semanticText).map((reference) => {
		const text = reference.text;
		return {
			id: stableHash(`${exchange.id}:${reference.kind}:${text}`),
			sessionPath: exchange.sessionPath,
			sessionId: exchange.sessionId,
			kind: reference.kind,
			sourceEntryId: exchange.sourceEntryId,
			titleText: "",
			bodyText: "",
			referenceText: normalizeSearchTerms(text),
			displayText: text,
			semanticText: "",
			contentStatus: exchange.contentStatus,
			sourceHash: stableHash(`${exchange.sourceHash}:${reference.kind}:${text}`),
			embedding: new Array(VECTOR_DIMENSIONS).fill(0),
			embeddingState: "failed",
			embeddingSourceHash: "",
			createdAt: exchange.createdAt,
			updatedAt: exchange.updatedAt,
		};
	});
}

async function firstRecordBatch<T>(query: AsyncIterable<T>): Promise<T | undefined> {
	for await (const batch of query) return batch;
	return undefined;
}

class LanceMemoryRepository {
	private database: Awaited<ReturnType<typeof lancedb.connect>> | undefined;
	private itemsTable: Awaited<ReturnType<Awaited<ReturnType<typeof lancedb.connect>>["openTable"]>> | undefined;
	private stateTable: Awaited<ReturnType<Awaited<ReturnType<typeof lancedb.connect>>["openTable"]>> | undefined;
	private vectorIndexBuild: Promise<void> | undefined;

	private readonly databasePath: string;

	constructor(databasePath: string) {
		this.databasePath = databasePath;
	}

	private async getDatabase(): Promise<NonNullable<typeof this.database>> {
		if (!this.database) {
			await mkdir(this.databasePath, { recursive: true, mode: 0o700 });
			this.database = await lancedb.connect(this.databasePath);
		}
		return this.database;
	}

	private async getItemsTable(seed?: readonly MemoryItem[]): Promise<NonNullable<typeof this.itemsTable> | undefined> {
		if (this.itemsTable) return this.itemsTable;
		const database = await this.getDatabase();
		try {
			this.itemsTable = await database.openTable(ITEMS_TABLE);
			return this.itemsTable;
		} catch {
			if (!seed || seed.length === 0) return undefined;
			this.itemsTable = await database.createTable(ITEMS_TABLE, seed as unknown as Record<string, unknown>[]);
			await this.ensureIndexes(this.itemsTable);
			return this.itemsTable;
		}
	}

	private async getStateTable(): Promise<NonNullable<typeof this.stateTable>> {
		if (this.stateTable) return this.stateTable;
		const database = await this.getDatabase();
		try {
			this.stateTable = await database.openTable(STATE_TABLE);
		} catch {
			this.stateTable = await database.createTable(STATE_TABLE, [
				{
					key: "memory-v1",
					sessionPath: "",
					fingerprint: MEMORY_VERSION,
					updatedAt: Date.now(),
				} satisfies MemoryStateRow,
			]);
		}
		return this.stateTable;
	}

	async initialize(): Promise<void> {
		await this.getStateTable();
	}

	private async ensureIndexes(table: NonNullable<typeof this.itemsTable>): Promise<void> {
		for (const column of ["titleText", "bodyText", "referenceText"] as const) {
			try {
				await table.createIndex(column, {
					config: lancedb.Index.fts({
						baseTokenizer: "whitespace",
						lowercase: false,
						withPosition: true,
						...(column === "titleText" ? { prefixOnly: true } : {}),
					}),
				});
			} catch {
				// Existing indexes are valid after a restart or concurrent local initializer.
			}
		}
		try {
			await table.createIndex("embeddingState", { config: lancedb.Index.bitmap() });
		} catch {
			// Existing bitmap index is valid.
		}
	}

	async replaceSession(sessionPath: string, items: readonly MemoryItem[]): Promise<void> {
		const existingTable = await this.getItemsTable();
		const existingRows = existingTable
			? ((await existingTable
					.query()
					.where(`sessionPath = ${quotePredicate(sessionPath)}`)
					.toArray()) as LanceMemoryRow[])
			: [];
		const existingById = new Map(
			existingRows.flatMap((row) => {
				const item = memoryItemFromRow(row);
				return item ? [[item.id, item] as const] : [];
			}),
		);
		const replacement = items.map((item) => this.preserveEmbedding(item, existingById.get(item.id)));
		const table = existingTable ?? (await this.getItemsTable(replacement));
		if (!table) return;
		await table.delete(`sessionPath = ${quotePredicate(sessionPath)}`);
		if (replacement.length > 0) await table.add(replacement as unknown as Record<string, unknown>[]);
	}

	private preserveEmbedding(item: MemoryItem, existing: MemoryItem | undefined): MemoryItem {
		if (
			!existing ||
			existing.sourceHash !== item.sourceHash ||
			existing.embeddingState !== "ready" ||
			existing.embeddingSourceHash !== item.sourceHash ||
			!isVerifiedEmbedding(existing.embedding)
		)
			return item;
		return {
			...item,
			embedding: existing.embedding,
			embeddingState: existing.embeddingState,
			embeddingSourceHash: existing.embeddingSourceHash,
		};
	}

	async upsert(item: MemoryItem): Promise<void> {
		const existingTable = await this.getItemsTable();
		const existingRows = existingTable
			? ((await existingTable
					.query()
					.where(`id = ${quotePredicate(item.id)}`)
					.toArray()) as LanceMemoryRow[])
			: [];
		const existing = existingRows.map(memoryItemFromRow).find((row): row is MemoryItem => Boolean(row));
		const replacement = this.preserveEmbedding(item, existing);
		const table = existingTable ?? (await this.getItemsTable([replacement]));
		if (!table || !existingTable) return;
		await table.delete(`id = ${quotePredicate(item.id)}`);
		await table.add([replacement] as unknown as Record<string, unknown>[]);
	}

	async deleteSessionTitle(sessionPath: string): Promise<void> {
		const table = await this.getItemsTable();
		if (!table) return;
		await table.delete(`sessionPath = ${quotePredicate(sessionPath)} AND kind = 'conversation-title'`);
	}

	async replaceExchangeReferences(exchange: MemoryItem): Promise<void> {
		const table = await this.getItemsTable();
		if (!table) return;
		await table.delete(
			`sessionPath = ${quotePredicate(exchange.sessionPath)} AND sourceEntryId = ${quotePredicate(exchange.sourceEntryId)} AND kind IN ('file-reference', 'command-reference')`,
		);
		const references = createReferenceItems(exchange);
		if (references.length > 0) await table.add(references as unknown as Record<string, unknown>[]);
	}

	async latestPendingExchange(sessionPath: string): Promise<MemoryItem | undefined> {
		const table = await this.getItemsTable();
		if (!table) return undefined;
		const rows = (await table
			.query()
			.where(
				`sessionPath = ${quotePredicate(sessionPath)} AND kind = 'conversation-exchange' AND contentStatus = 'pending'`,
			)
			.select([
				"id",
				"sessionPath",
				"sessionId",
				"kind",
				"sourceEntryId",
				"titleText",
				"bodyText",
				"referenceText",
				"displayText",
				"semanticText",
				"contentStatus",
				"sourceHash",
				"embedding",
				"embeddingState",
				"embeddingSourceHash",
				"createdAt",
				"updatedAt",
			])
			.toArray()) as LanceMemoryRow[];
		return rows
			.map(memoryItemFromRow)
			.filter((item): item is MemoryItem => Boolean(item))
			.sort((left, right) => right.updatedAt - left.updatedAt)[0];
	}

	async pendingEmbeddings(limit: number): Promise<PendingEmbedding[]> {
		const table = await this.getItemsTable();
		if (!table) return [];
		const rows = (await table
			.query()
			.where("embeddingState = 'pending' AND kind = 'conversation-exchange'")
			.select(["id", "sourceHash", "semanticText"])
			.limit(limit)
			.toArray()) as LanceMemoryRow[];
		return rows.flatMap((row) => {
			const id = asString(row.id);
			const sourceHash = asString(row.sourceHash);
			const semanticText = asString(row.semanticText);
			return id && sourceHash && semanticText ? [{ id, sourceHash, semanticText }] : [];
		});
	}

	async saveEmbedding(entry: PendingEmbedding, vector: Float32Array): Promise<boolean> {
		if (!isVerifiedEmbedding(vector)) return false;
		const table = await this.getItemsTable();
		if (!table) return false;
		await table.update({
			where: `id = ${quotePredicate(entry.id)} AND sourceHash = ${quotePredicate(entry.sourceHash)}`,
			values: {
				embedding: [...vector],
				embeddingState: "ready",
				embeddingSourceHash: entry.sourceHash,
			},
		});
		return true;
	}

	async markEmbeddingFailed(entry: PendingEmbedding): Promise<void> {
		const table = await this.getItemsTable();
		if (!table) return;
		await table.update({
			where: `id = ${quotePredicate(entry.id)} AND sourceHash = ${quotePredicate(entry.sourceHash)}`,
			values: { embeddingState: "failed" },
		});
	}

	async deleteSession(sessionPath: string): Promise<void> {
		const table = await this.getItemsTable();
		if (table) await table.delete(`sessionPath = ${quotePredicate(sessionPath)}`);
		const state = await this.getStateTable();
		await state.delete(`sessionPath = ${quotePredicate(sessionPath)}`);
	}

	async deleteSessionsExcept(sessionPaths: readonly string[]): Promise<void> {
		const table = await this.getItemsTable();
		if (table) {
			if (sessionPaths.length === 0) await table.delete("true");
			else await table.delete(`sessionPath NOT IN (${sessionPaths.map(quotePredicate).join(", ")})`);
		}
		const state = await this.getStateTable();
		if (sessionPaths.length === 0) await state.delete("sessionPath != ''");
		else
			await state.delete(
				`sessionPath != '' AND sessionPath NOT IN (${sessionPaths.map(quotePredicate).join(", ")})`,
			);
	}

	private stateKey(sessionPath: string): string {
		return `session:${stableHash(sessionPath)}`;
	}

	async fingerprintFor(sessionPath: string): Promise<string | undefined> {
		const table = await this.getStateTable();
		const rows = (await table
			.query()
			.where(`key = ${quotePredicate(this.stateKey(sessionPath))}`)
			.toArray()) as Array<{
			fingerprint?: unknown;
		}>;
		return asString(rows[0]?.fingerprint);
	}

	async writeFingerprint(sessionPath: string, fingerprint: string): Promise<void> {
		const table = await this.getStateTable();
		const key = this.stateKey(sessionPath);
		await table.delete(`key = ${quotePredicate(key)}`);
		await table.add([{ key, sessionPath, fingerprint, updatedAt: Date.now() } satisfies MemoryStateRow]);
	}

	async searchLexical(query: string, limit: number): Promise<MemorySearchResult[]> {
		const table = await this.getItemsTable();
		if (!table) return [];
		const normalized = normalizeSearchTerms(query);
		if (!normalized) return [];
		const codeLike = isCodeLikeSearchQuery(query);
		const fts = new lancedb.MultiMatchQuery(normalized, ["titleText", "bodyText", "referenceText"], {
			boosts: codeLike ? [8, 3, 9] : [9, 4, 5],
			operator: codeLike ? lancedb.Operator.Or : lancedb.Operator.And,
		});
		const rows = (await table
			.search(fts)
			.limit(limit * 3)
			.toArray()) as LanceMemoryRow[];
		return this.aggregate(rows, "lexical", limit);
	}

	async searchHybrid(query: string, vector: Float32Array, limit: number): Promise<MemorySearchResult[]> {
		const table = await this.getItemsTable();
		if (!table) return [];
		const normalized = normalizeSearchTerms(query);
		if (!normalized) return [];
		const codeLike = isCodeLikeSearchQuery(query);
		const fts = new lancedb.MultiMatchQuery(normalized, ["titleText", "bodyText", "referenceText"], {
			boosts: codeLike ? [8, 3, 9] : [9, 4, 5],
			operator: codeLike ? lancedb.Operator.Or : lancedb.Operator.And,
		});
		const [ftsBatch, vectorBatch] = await Promise.all([
			firstRecordBatch(
				table
					.search(fts)
					.withRowId()
					.limit(limit * 3),
			),
			firstRecordBatch(
				table
					.vectorSearch(vector)
					.where("embeddingState = 'ready'")
					.distanceType("cosine")
					.withRowId()
					.limit(limit * 3),
			),
		]);
		if (!ftsBatch && !vectorBatch) return [];
		if (!ftsBatch) return this.aggregate((vectorBatch?.toArray() ?? []) as LanceMemoryRow[], "semantic", limit);
		if (!vectorBatch) return this.aggregate(ftsBatch.toArray() as LanceMemoryRow[], "lexical", limit);
		const reranker = await lancedb.rerankers.RRFReranker.create();
		return this.aggregate(
			(await reranker.rerankHybrid(query, vectorBatch, ftsBatch)).toArray() as LanceMemoryRow[],
			"hybrid",
			limit,
		);
	}

	async ensureVectorIndex(): Promise<void> {
		if (this.vectorIndexBuild) return await this.vectorIndexBuild;
		this.vectorIndexBuild = (async () => {
			const table = await this.getItemsTable();
			if (!table || (await table.countRows("embeddingState = 'ready'")) < ANN_INDEX_THRESHOLD) return;
			const indexes = await table.listIndices();
			if (indexes.some((index) => index.name === VECTOR_INDEX_NAME || index.columns.includes("embedding"))) return;
			await table.createIndex("embedding", {
				name: VECTOR_INDEX_NAME,
				config: lancedb.Index.hnswSq({ distanceType: "cosine", numPartitions: 1, m: 16, efConstruction: 200 }),
			});
		})().finally(() => {
			this.vectorIndexBuild = undefined;
		});
		return await this.vectorIndexBuild;
	}

	/** Read only table metadata and scalar counts for `/status`. */
	async getDiagnostics(): Promise<MemoryRepositoryDiagnostics> {
		const table = await this.getItemsTable();
		if (!table) {
			return {
				conversations: 0,
				exchanges: 0,
				embeddings: { pending: 0, ready: 0, failed: 0 },
				vectorIndex: "flat",
			};
		}
		const [exchangeRows, pending, ready, failed, indexes] = await Promise.all([
			table.query().where("kind = 'conversation-exchange'").select(["sessionPath"]).toArray(),
			table.countRows("kind = 'conversation-exchange' AND embeddingState = 'pending'"),
			table.countRows("kind = 'conversation-exchange' AND embeddingState = 'ready'"),
			table.countRows("kind = 'conversation-exchange' AND embeddingState = 'failed'"),
			table.listIndices(),
		]);
		const conversations = new Set(
			(exchangeRows as LanceMemoryRow[])
				.map((row) => asString(row.sessionPath))
				.filter((sessionPath): sessionPath is string => Boolean(sessionPath)),
		).size;
		return {
			conversations,
			exchanges: pending + ready + failed,
			embeddings: { pending, ready, failed },
			vectorIndex: indexes.some((index) => index.name === VECTOR_INDEX_NAME || index.columns.includes("embedding"))
				? "hnsw-sq"
				: "flat",
		};
	}

	private aggregate(
		rows: readonly LanceMemoryRow[],
		mode: "lexical" | "semantic" | "hybrid",
		limit: number,
	): MemorySearchResult[] {
		const bySession = new Map<string, MemorySearchResult>();
		for (const row of rows) {
			const item = memoryItemFromRow(row);
			if (!item) continue;
			const kind = evidenceKind(item.kind, mode === "semantic");
			const rank =
				mode === "semantic"
					? 1 - (asNumber(row._distance) ?? 1)
					: mode === "hybrid"
						? (asNumber(row._relevance_score) ?? 0)
						: (asNumber(row._score) ?? 0);
			const candidate: MemorySearchResult = {
				sessionPath: item.sessionPath,
				score: mode === "hybrid" ? rank : rank + scoreForKind(kind),
				evidence: { kind, label: labelForKind(kind), snippet: normalizeSearchPreview(item.displayText, 180) },
			};
			const existing = bySession.get(item.sessionPath);
			if (!existing || candidate.score > existing.score) bySession.set(item.sessionPath, candidate);
		}
		return [...bySession.values()].sort((left, right) => right.score - left.score).slice(0, limit);
	}

	async close(): Promise<void> {
		this.itemsTable?.close();
		this.itemsTable = undefined;
		this.stateTable?.close();
		this.stateTable = undefined;
		this.database = undefined;
	}

	async reset(): Promise<void> {
		await this.close();
		await rm(this.databasePath, { recursive: true, force: true });
	}
}

class MemoryEmbeddingController {
	private releaseLock: (() => Promise<void>) | undefined;
	private pollTimer: NodeJS.Timeout | undefined;
	private pollPromise: Promise<void> | undefined;
	private stopped = false;
	private started = false;
	private lockAttempted = false;
	private delay = INITIAL_POLL_DELAY_MS;
	private modelUnavailable = false;

	private readonly repository: LanceMemoryRepository;
	private readonly engine: LocalEmbeddingEngine;
	private readonly lockPath: string;
	private readonly onUnavailable: (error: Error) => void;
	private readonly onReady: () => void;
	private readonly onWorkComplete: () => void;

	constructor(
		repository: LanceMemoryRepository,
		engine: LocalEmbeddingEngine,
		lockPath: string,
		onUnavailable: (error: Error) => void,
		onReady: () => void,
		onWorkComplete: () => void,
	) {
		this.repository = repository;
		this.engine = engine;
		this.lockPath = lockPath;
		this.onUnavailable = onUnavailable;
		this.onReady = onReady;
		this.onWorkComplete = onWorkComplete;
	}

	async start(): Promise<void> {
		this.started = true;
		await mkdir(dirname(this.lockPath), { recursive: true, mode: 0o700 });
		await writeFile(this.lockPath, "", { flag: "a", mode: 0o600 });
		this.lockAttempted = true;
		try {
			this.releaseLock = await lockfile.lock(this.lockPath, {
				realpath: false,
				stale: 120_000,
				retries: 0,
			});
		} catch {
			return;
		}
		this.onReady();
		this.schedule(0);
	}

	getStatus(): MemoryEmbeddingWorkerState {
		if (this.stopped || !this.started) return "not-started";
		if (this.modelUnavailable) return "unavailable";
		if (!this.lockAttempted) return "starting";
		if (!this.releaseLock) return "another-process";
		return this.pollPromise ? "active" : "idle";
	}

	private schedule(delay: number): void {
		if (this.stopped || this.modelUnavailable || !this.releaseLock) return;
		this.pollTimer = setTimeout(() => {
			this.pollPromise = this.poll().finally(() => {
				this.pollPromise = undefined;
			});
		}, delay);
	}

	private async poll(): Promise<void> {
		if (this.stopped || this.modelUnavailable) return;
		try {
			const pending = await this.repository.pendingEmbeddings(POLL_BATCH_SIZE);
			if (pending.length === 0) {
				this.delay = Math.min(MAX_POLL_DELAY_MS, this.delay * 2);
				this.schedule(this.delay);
				return;
			}
			this.delay = INITIAL_POLL_DELAY_MS;
			const vectors = await this.engine.embedDocuments(pending.map((item) => item.semanticText));
			for (const [index, item] of pending.entries()) {
				const vector = vectors[index];
				if (!vector || !(await this.repository.saveEmbedding(item, vector)))
					await this.repository.markEmbeddingFailed(item);
			}
			await this.repository.ensureVectorIndex();
			this.onWorkComplete();
			this.schedule(0);
		} catch (error) {
			this.modelUnavailable = true;
			this.onUnavailable(error instanceof Error ? error : new Error(String(error)));
		}
	}

	async dispose(): Promise<void> {
		this.stopped = true;
		if (this.pollTimer) clearTimeout(this.pollTimer);
		this.pollTimer = undefined;
		await this.pollPromise;
		if (this.releaseLock) await this.releaseLock().catch(() => {});
		this.releaseLock = undefined;
	}
}

function deriveIndexingDiagnostics(options: {
	store: MemoryStoreState;
	semantic: MemoryRuntimeStatus;
	worker: MemoryEmbeddingWorkerState;
	engine: LocalEmbeddingEngineDiagnostics;
	embeddings: Record<MemoryEmbeddingState, number>;
}): MemoryIndexingDiagnostics {
	const pending = options.embeddings.pending;
	const active = options.engine.activeDocuments;
	if (options.store !== "ready") {
		return { state: options.store === "unavailable" ? "unavailable" : "starting", pending, active };
	}
	if (
		options.semantic.phase === "unavailable" ||
		options.worker === "unavailable" ||
		options.engine.phase === "failed"
	)
		return { state: "unavailable", pending, active };
	if (options.worker === "another-process") return { state: "another-process", pending, active };
	if (options.engine.phase === "embedding" && active > 0) return { state: "embedding", pending, active };
	if (options.worker === "not-started" || options.worker === "starting") return { state: "starting", pending, active };
	if (pending === 0) return { state: "up-to-date", pending, active };
	if (options.engine.phase === "not-started" || options.engine.phase === "loading")
		return { state: "starting", pending, active };
	if (pending > 0) return { state: "queued", pending, active };
	return { state: "up-to-date", pending, active };
}

/**
 * Workspace-local memory materialization and retrieval. JSONL remains the
 * source of truth; this class never scans JSONL from a Side search request.
 */
export class MemoryRuntime {
	private readonly repository: LanceMemoryRepository;
	private readonly embeddingEngine: LocalEmbeddingEngine;
	private readonly controller: MemoryEmbeddingController;
	private readonly agentDir: string;
	private readonly usesLocalEmbeddingAssets: boolean;
	private readonly pendingBySession = new Map<string, PendingExchange[]>();
	/**
	 * A title is mutable session metadata rather than immutable conversation
	 * content. This map is the authoritative value between a session_info write
	 * and the next sidebar refresh, and lets us reject a stale Lance FTS title
	 * hit without rebuilding indexes on every rename.
	 */
	private readonly currentTitles = new Map<string, string>();
	private started = false;
	private startup: Promise<void> | undefined;
	private store: MemoryStoreState = "preparing";
	private status: MemoryRuntimeStatus = { phase: "preparing", message: "Preparing local memory…" };
	private semanticReady = false;
	private readonly onStatus: ((status: MemoryRuntimeStatus) => void) | undefined;
	private readonly onSearchRefresh: (() => void) | undefined;

	constructor(options: {
		agentDir: string;
		cwd: string;
		embeddingEngine?: LocalEmbeddingEngine;
		onStatus?: (status: MemoryRuntimeStatus) => void;
		onEmbeddingStatus?: (status: LocalEmbeddingStatus | undefined) => void;
		onSearchRefresh?: () => void;
	}) {
		const databasePath = getMemoryDatabasePath(options.agentDir, options.cwd);
		this.agentDir = options.agentDir;
		this.onStatus = options.onStatus;
		this.onSearchRefresh = options.onSearchRefresh;
		this.repository = new LanceMemoryRepository(databasePath);
		this.usesLocalEmbeddingAssets = options.embeddingEngine === undefined;
		this.embeddingEngine =
			options.embeddingEngine ??
			new LocalEmbeddingWorker(options.agentDir, {
				onStatus: (status) => {
					if (status.phase === "ready") this.semanticReady = true;
					options.onEmbeddingStatus?.(status);
				},
			});
		this.controller = new MemoryEmbeddingController(
			this.repository,
			this.embeddingEngine,
			join(databasePath, EMBEDDING_LOCK_FILE),
			(error) => {
				this.setStatus({ phase: "unavailable", message: `Local semantic search unavailable: ${error.message}` });
			},
			() => {
				this.semanticReady = true;
				this.onSearchRefresh?.();
			},
			() => this.onSearchRefresh?.(),
		);
	}

	private setStatus(status: MemoryRuntimeStatus): void {
		this.status = status;
		this.onStatus?.(status);
	}

	getStatus(): MemoryRuntimeStatus {
		return this.status;
	}

	async getDiagnostics(): Promise<MemoryRuntimeDiagnostics> {
		const empty = {
			conversations: 0,
			exchanges: 0,
			embeddings: { pending: 0, ready: 0, failed: 0 },
			vectorIndex: "flat" as const,
		};
		const engine =
			this.embeddingEngine.getDiagnostics?.() ??
			({
				phase: "not-started",
				runtime: "same-process-worker-thread",
				pendingQueries: 0,
				pendingDocuments: 0,
				activeDocuments: 0,
			} satisfies LocalEmbeddingEngineDiagnostics);
		const worker = this.controller.getStatus();
		if (this.store !== "ready") {
			return {
				store: this.store,
				...empty,
				worker,
				engine,
				indexing: deriveIndexingDiagnostics({
					store: this.store,
					semantic: this.status,
					worker,
					engine,
					embeddings: empty.embeddings,
				}),
				semantic: this.status,
			};
		}
		try {
			const repository = await this.repository.getDiagnostics();
			return {
				store: "ready",
				...repository,
				worker,
				engine,
				indexing: deriveIndexingDiagnostics({
					store: "ready",
					semantic: this.status,
					worker,
					engine,
					embeddings: repository.embeddings,
				}),
				semantic: this.status,
			};
		} catch (error) {
			const semantic: MemoryRuntimeStatus = {
				phase: "unavailable",
				message: `Memory store unavailable: ${error instanceof Error ? error.message : String(error)}`,
			};
			return {
				store: "unavailable",
				...empty,
				worker,
				engine,
				indexing: deriveIndexingDiagnostics({
					store: "unavailable",
					semantic,
					worker,
					engine,
					embeddings: empty.embeddings,
				}),
				semantic,
			};
		}
	}

	async start(sessions: readonly SessionInfo[]): Promise<void> {
		if (this.startup) return await this.startup;
		this.started = true;
		this.startup = this.startInternal(sessions);
		return await this.startup;
	}

	private async startInternal(sessions: readonly SessionInfo[]): Promise<void> {
		try {
			await this.repository.initialize();
			await this.reconcile(sessions);
			this.store = "ready";
			await this.startEmbeddingController();
		} catch (initialError) {
			try {
				await this.repository.reset();
				await this.repository.initialize();
				await this.reconcile(sessions);
				this.store = "ready";
				await this.startEmbeddingController();
			} catch (recoveryError) {
				this.store = "unavailable";
				const message = recoveryError instanceof Error ? recoveryError.message : String(recoveryError);
				const initialMessage = initialError instanceof Error ? initialError.message : String(initialError);
				this.setStatus({ phase: "unavailable", message: `Memory store unavailable: ${message || initialMessage}` });
			}
		}
	}

	private async startEmbeddingController(): Promise<void> {
		if (!this.usesLocalEmbeddingAssets) {
			await this.embeddingEngine.prepare();
			this.semanticReady = true;
			this.setStatus({ phase: "ready" });
			void this.controller.start();
			return;
		}
		const availability = await getLocalEmbeddingAvailability(this.agentDir);
		if (availability.state === "ready") {
			this.setStatus({ phase: "preparing", message: "Loading local semantic search…" });
			try {
				await this.embeddingEngine.prepare();
			} catch (error) {
				this.semanticReady = false;
				this.setStatus({
					phase: "unavailable",
					message: `Local semantic search unavailable: ${error instanceof Error ? error.message : String(error)}`,
				});
				return;
			}
			this.semanticReady = true;
			this.setStatus({ phase: "ready" });
			void this.controller.start();
			return;
		}
		this.semanticReady = false;
		this.setStatus({
			phase: "unavailable",
			message:
				availability.state === "missing"
					? "Keyword search · semantic model not installed. Run bone setup."
					: `Keyword search · semantic model needs repair (${availability.reason}). Run bone setup.`,
		});
	}

	async recordPersistedEntries(
		session: { path: string; id: string; name?: string },
		entries: readonly SessionEntry[],
	): Promise<void> {
		if (!this.started || !session.path.endsWith(".jsonl")) return;
		for (const entry of entries) {
			if (entry.type === "session_info") {
				await this.recordTitle(session, entry.name ?? "", Date.parse(entry.timestamp));
				continue;
			}
			if (entry.type !== "message" || !isMessageWithContent(entry.message) || entry.message.role !== "user")
				continue;
			const userText = normalizeSearchPreview(textFromMessage(entry.message), 2_000);
			if (!userText) continue;
			const exchange = createExchangeItem({
				sessionPath: session.path,
				sessionId: session.id,
				sourceEntryId: entry.id,
				userText,
				createdAt: Date.parse(entry.timestamp),
			});
			await this.repository.upsert(exchange);
			await this.repository.replaceExchangeReferences(exchange);
			const pending = this.pendingBySession.get(session.path) ?? [];
			if (!pending.some((item) => item.id === exchange.id)) {
				pending.push({ id: exchange.id, sourceEntryId: entry.id, userText });
				this.pendingBySession.set(session.path, pending);
			}
		}
	}

	async recordCompletedRun(
		session: { path: string; id: string; name?: string },
		messages: readonly AgentMessage[],
	): Promise<void> {
		if (!this.started || !session.path.endsWith(".jsonl")) return;
		const assistant = [...messages].reverse().find(isFinalAssistantMessage);
		if (!assistant) return;
		let pending = this.pendingBySession.get(session.path)?.at(-1);
		if (!pending) {
			const latest = await this.repository.latestPendingExchange(session.path);
			if (!latest) return;
			pending = {
				id: latest.id,
				sourceEntryId: latest.sourceEntryId,
				userText:
					latest.semanticText.replace(/^User task:\s*/, "").split("\nFinal result:")[0] ?? latest.displayText,
			};
		}
		const assistantText = normalizeSearchPreview(textFromMessage(assistant), 4_000);
		if (!assistantText) return;
		const exchange = createExchangeItem({
			sessionPath: session.path,
			sessionId: session.id,
			sourceEntryId: pending.sourceEntryId,
			userText: pending.userText,
			assistantText,
			createdAt: Date.now(),
		});
		await this.repository.upsert(exchange);
		await this.repository.replaceExchangeReferences(exchange);
		const queue = this.pendingBySession.get(session.path);
		if (queue) {
			const index = queue.findIndex((item) => item.id === pending.id);
			if (index >= 0) queue.splice(index, 1);
			if (queue.length === 0) this.pendingBySession.delete(session.path);
		}
	}

	async recordTitle(session: { path: string; id: string }, title: string, updatedAt = Date.now()): Promise<void> {
		if (!this.started || !session.path.endsWith(".jsonl")) return;
		this.currentTitles.set(session.path, title);
		const item = createTitleItem({ sessionPath: session.path, sessionId: session.id, title, updatedAt });
		if (item) await this.repository.upsert(item);
		else await this.repository.deleteSessionTitle(session.path);
	}

	async removeSession(sessionPath: string): Promise<void> {
		this.pendingBySession.delete(sessionPath);
		this.currentTitles.delete(sessionPath);
		await this.repository.deleteSession(sessionPath);
	}

	async search(query: string, sessions: readonly SessionInfo[], limit = 30): Promise<MemorySearchResult[]> {
		if (!query.trim()) return [];
		const indexed = await this.repository.searchLexical(query, limit);
		return this.mergeLiveOverlay(query, sessions, indexed, limit);
	}

	async searchSemantic(query: string, sessions: readonly SessionInfo[], limit = 30): Promise<MemorySearchResult[]> {
		if (!query.trim() || this.status.phase !== "ready" || !this.semanticReady)
			return await this.search(query, sessions, limit);
		try {
			const indexed = await this.repository.searchHybrid(query, await this.embeddingEngine.embedQuery(query), limit);
			return this.mergeLiveOverlay(query, sessions, indexed, limit);
		} catch {
			return await this.search(query, sessions, limit);
		}
	}

	private async reconcile(sessions: readonly SessionInfo[]): Promise<void> {
		const persisted = sessions.filter((session) => session.path.endsWith(".jsonl") && existsSync(session.path));
		for (const session of persisted) {
			if (!this.currentTitles.has(session.path))
				this.currentTitles.set(session.path, session.name ?? normalizeSearchPreview(session.firstMessage, 120));
			const sessionStat = await stat(session.path);
			const fingerprint = `${sessionStat.mtimeMs}:${sessionStat.size}`;
			if ((await this.repository.fingerprintFor(session.path)) === fingerprint) continue;
			const items = await extractMemoryItems(session);
			await this.repository.replaceSession(session.path, items);
			await this.repository.writeFingerprint(session.path, fingerprint);
		}
		await this.repository.deleteSessionsExcept(persisted.map((session) => session.path));
	}

	private mergeLiveOverlay(
		query: string,
		sessions: readonly SessionInfo[],
		indexed: readonly MemorySearchResult[],
		limit: number,
	): MemorySearchResult[] {
		const queryTerms = normalizeSearchTerms(query).split(" ").filter(Boolean);
		const codeLike = isCodeLikeSearchQuery(query);
		const sessionsByPath = new Map(sessions.map((session) => [session.path, session]));
		const titleFor = (session: SessionInfo): string =>
			this.currentTitles.get(session.path) ?? session.name ?? normalizeSearchPreview(session.firstMessage, 120);
		const matchesTitleQuery = (text: string): boolean => {
			const terms = normalizeSearchTerms(text);
			return (
				queryTerms.length > 0 &&
				(codeLike
					? queryTerms.some((term) => terms.includes(term))
					: queryTerms.every((term) => terms.includes(term)))
			);
		};

		// Lance FTS compacts mutable documents asynchronously. A renamed title can
		// therefore be returned briefly for its previous tokens. Only title
		// evidence is metadata-driven, so verify it against the current title
		// before presenting it; body, file, and semantic evidence remain intact.
		const results = new Map(
			indexed.flatMap((result) => {
				const session = sessionsByPath.get(result.sessionPath);
				if (result.evidence.kind === "title" && session && !matchesTitleQuery(titleFor(session))) return [];
				return [[result.sessionPath, result] as const];
			}),
		);
		for (const session of sessions) {
			const title = titleFor(session);
			const titleTerms = normalizeSearchTerms(title);
			if (matchesTitleQuery(title)) {
				const candidate: MemorySearchResult = {
					sessionPath: session.path,
					score: 20 + queryTerms.length,
					evidence: {
						kind: "title",
						label: "Title",
						snippet: normalizeSearchPreview(title, 180),
					},
				};
				const existing = results.get(session.path);
				if (!existing || candidate.score > existing.score) results.set(session.path, candidate);
				continue;
			}
			if (results.has(session.path) || existsSync(session.path)) continue;
			const body = `${session.firstMessage} ${session.lastMessage ?? ""}`;
			const bodyTerms = normalizeSearchTerms(body);
			const matched = queryTerms.filter((term) => titleTerms.includes(term) || bodyTerms.includes(term)).length;
			if (matched === 0) continue;
			const titleMatch = queryTerms.some((term) => titleTerms.includes(term));
			results.set(session.path, {
				sessionPath: session.path,
				score: matched + (titleMatch ? 8 : 3),
				evidence: {
					kind: titleMatch ? "title" : "user",
					label: titleMatch ? "Title" : "You",
					snippet: normalizeSearchPreview(titleMatch ? title : body, 180),
				},
			});
		}
		return [...results.values()].sort((left, right) => right.score - left.score).slice(0, limit);
	}

	async dispose(): Promise<void> {
		await this.controller.dispose();
		await this.embeddingEngine.dispose();
		await this.repository.close();
	}
}

export async function extractMemoryItems(session: SessionInfo): Promise<MemoryItem[]> {
	if (!existsSync(session.path)) return [];
	const raw = await readFile(session.path, "utf8");
	const items: MemoryItem[] = [];
	let title = session.name?.trim() || "";
	let sessionId = session.id;
	const pending: PendingExchange[] = [];
	for (const line of raw.split(/\r?\n/)) {
		const entry = parseSessionEntryLine(line);
		if (!entry) continue;
		if (entry.type === "session") {
			sessionId = entry.id;
			continue;
		}
		if (entry.type === "session_info") {
			title = entry.name?.trim() || "";
			continue;
		}
		if (entry.type !== "message" || !isMessageWithContent(entry.message)) continue;
		const text = normalizeSearchPreview(textFromMessage(entry.message), 4_000);
		if (!text) continue;
		if (entry.message.role === "user") {
			pending.push({
				id: stableHash(`${session.path}:exchange:${entry.id}`),
				sourceEntryId: entry.id,
				userText: text,
			});
			continue;
		}
		if (!isFinalAssistantMessage(entry.message)) continue;
		const user = pending.pop();
		if (!user) continue;
		const exchange = createExchangeItem({
			sessionPath: session.path,
			sessionId,
			sourceEntryId: user.sourceEntryId,
			userText: user.userText,
			assistantText: text,
			createdAt: Date.parse(entry.timestamp),
		});
		items.push(exchange, ...createReferenceItems(exchange));
	}
	for (const user of pending) {
		const exchange = createExchangeItem({
			sessionPath: session.path,
			sessionId,
			sourceEntryId: user.sourceEntryId,
			userText: user.userText,
			createdAt: session.modified.getTime(),
		});
		items.push(exchange, ...createReferenceItems(exchange));
	}
	const titleItem = createTitleItem({
		sessionPath: session.path,
		sessionId,
		title: title || normalizeSearchPreview(session.firstMessage, 120),
		updatedAt: session.modified.getTime(),
	});
	if (titleItem) items.unshift(titleItem);
	return items;
}

export function getMemoryDatabasePath(agentDir: string, cwd: string): string {
	return join(agentDir, "memory", MEMORY_VERSION, stableHash(cwd));
}
