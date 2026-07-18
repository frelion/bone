import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import * as lancedb from "@lancedb/lancedb";
import { type LocalEmbeddingEngine, type LocalEmbeddingStatus, LocalEmbeddingWorker } from "./local-embedding.ts";
import {
	isMessageWithContent,
	parseSessionEntryLine,
	type SessionInfo,
	type SessionMessageEntry,
} from "./session-manager.ts";
import { isCodeLikeSearchQuery, normalizeSearchPreview, normalizeSearchTerms } from "./session-search-normalizer.ts";

const VECTOR_DIMENSIONS = 384;
const TABLE_NAME = "session_documents";
const STATE_TABLE_NAME = "search_state";
const SEARCH_INDEX_VERSION = "2";
const ANN_INDEX_THRESHOLD = 10_000;
const VECTOR_INDEX_NAME = "embedding_hnsw_sq";

export type SessionSearchEvidenceKind = "title" | "user" | "assistant" | "reference" | "semantic";

export interface SessionSearchResult {
	sessionPath: string;
	score: number;
	evidence: {
		kind: SessionSearchEvidenceKind;
		label: "Title" | "You" | "Bone" | "File" | "Related discussion";
		snippet: string;
	};
}

export interface SearchDocument {
	documentId: string;
	sessionPath: string;
	kind: Exclude<SessionSearchEvidenceKind, "semantic">;
	titleText: string;
	bodyText: string;
	referenceText: string;
	displayText: string;
	semanticText: string;
	contentHash: string;
	updatedAt: number;
}

interface StoredSearchDocument extends SearchDocument {
	embedding: number[];
	embeddingReady: boolean;
}

interface LanceSearchRow {
	documentId?: unknown;
	sessionPath?: unknown;
	kind?: unknown;
	displayText?: unknown;
	titleText?: unknown;
	bodyText?: unknown;
	referenceText?: unknown;
	semanticText?: unknown;
	contentHash?: unknown;
	embedding?: unknown;
	embeddingReady?: unknown;
	_score?: unknown;
	_distance?: unknown;
}

interface SearchStateRow {
	key: "session-search";
	indexVersion: string;
	vectorIndex: "flat" | "hnsw-sq";
	updatedAt: number;
}

function stableHash(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function quotePredicate(value: string): string {
	return `'${value.replaceAll("'", "''")}'`;
}

function textFromMessage(message: AgentMessage): string {
	if (!isMessageWithContent(message)) return "";
	if (typeof message.content === "string") return message.content;
	return message.content
		.filter((part): part is { type: "text"; text: string } => part.type === "text")
		.map((part) => part.text)
		.join("\n");
}

function extractReferences(text: string): string[] {
	const references = new Set<string>();
	for (const match of text.matchAll(
		/(?:[A-Za-z0-9_.-]+[\\/])+[A-Za-z0-9_.-]+|\b[A-Za-z0-9_.-]+\.(?:ts|tsx|js|jsx|json|md|yaml|yml|toml|css|sh)\b/g,
	)) {
		references.add(match[0]);
	}
	for (const match of text.matchAll(/\b(?:npm|pnpm|bun|git|bone)\s+[A-Za-z0-9:_./-]+/g)) {
		references.add(match[0]);
	}
	return [...references];
}

function labelForKind(kind: SessionSearchEvidenceKind): SessionSearchResult["evidence"]["label"] {
	switch (kind) {
		case "title":
			return "Title";
		case "user":
			return "You";
		case "assistant":
			return "Bone";
		case "reference":
			return "File";
		case "semantic":
			return "Related discussion";
	}
}

function scoreForKind(kind: SessionSearchEvidenceKind): number {
	switch (kind) {
		case "title":
			return 8;
		case "reference":
			return 6;
		case "user":
			return 4;
		case "assistant":
			return 3;
		case "semantic":
			return 2;
	}
}

function asString(value: unknown): string | undefined {
	return typeof value === "string" ? value : undefined;
}

function asNumber(value: unknown): number | undefined {
	return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function asEmbedding(value: unknown): number[] | undefined {
	if (!Array.isArray(value) || !value.every((entry) => typeof entry === "number" && Number.isFinite(entry)))
		return undefined;
	return value;
}

function asKind(value: unknown): SearchDocument["kind"] | undefined {
	return value === "title" || value === "user" || value === "assistant" || value === "reference" ? value : undefined;
}

async function firstRecordBatch<T>(query: AsyncIterable<T>): Promise<T | undefined> {
	for await (const batch of query) return batch;
	return undefined;
}

function isFinalAssistantEntry(entry: SessionMessageEntry): boolean {
	const message = entry.message;
	return isMessageWithContent(message) && message.role === "assistant" && message.stopReason === "stop";
}

/** Extract the user-facing task memory from a compatible JSONL session file. */
export async function extractSessionSearchDocuments(session: SessionInfo): Promise<SearchDocument[]> {
	if (!existsSync(session.path)) return [];
	const raw = await readFile(session.path, "utf8");
	let title = session.name?.trim() || "";
	let pendingUser: { id: string; text: string } | undefined;
	const documents: SearchDocument[] = [];

	for (const line of raw.split(/\r?\n/)) {
		const entry = parseSessionEntryLine(line);
		if (!entry) continue;
		if (entry.type === "session_info") {
			title = entry.name?.trim() || "";
			continue;
		}
		if (entry.type !== "message") continue;
		const message = entry.message;
		if (!isMessageWithContent(message)) continue;
		const text = normalizeSearchPreview(textFromMessage(message), 2_000);
		if (!text) continue;

		if (message.role === "user") {
			pendingUser = { id: entry.id, text };
			continue;
		}
		if (!pendingUser || !isFinalAssistantEntry(entry)) continue;

		const references = extractReferences(`${pendingUser.text}\n${text}`);
		const body = `User task: ${pendingUser.text}\nFinal result: ${text}`;
		const documentId = stableHash(`${session.path}:exchange:${pendingUser.id}:${entry.id}`);
		documents.push({
			documentId,
			sessionPath: session.path,
			kind: "user",
			titleText: normalizeSearchTerms(title),
			bodyText: normalizeSearchTerms(body),
			referenceText: normalizeSearchTerms(references.join(" ")),
			displayText: pendingUser.text,
			semanticText: body,
			contentHash: stableHash(body),
			updatedAt: session.modified.getTime(),
		});
		if (references.length > 0) {
			documents.push({
				documentId: stableHash(`${session.path}:reference:${pendingUser.id}:${entry.id}`),
				sessionPath: session.path,
				kind: "reference",
				titleText: normalizeSearchTerms(title),
				bodyText: "",
				referenceText: normalizeSearchTerms(references.join(" ")),
				displayText: references.join(" · "),
				semanticText: references.join(" "),
				contentHash: stableHash(references.join("\n")),
				updatedAt: session.modified.getTime(),
			});
		}
		pendingUser = undefined;
	}

	const fallbackTitle = title || normalizeSearchPreview(session.firstMessage, 120);
	if (fallbackTitle) {
		documents.unshift({
			documentId: stableHash(`${session.path}:title`),
			sessionPath: session.path,
			kind: "title",
			titleText: normalizeSearchTerms(fallbackTitle),
			bodyText: "",
			referenceText: "",
			displayText: fallbackTitle,
			semanticText: fallbackTitle,
			contentHash: stableHash(fallbackTitle),
			updatedAt: session.modified.getTime(),
		});
	}

	return documents;
}

class LanceSessionSearchRepository {
	private database: Awaited<ReturnType<typeof lancedb.connect>> | undefined;
	private table: Awaited<ReturnType<Awaited<ReturnType<typeof lancedb.connect>>["openTable"]>> | undefined;
	private stateTable: Awaited<ReturnType<Awaited<ReturnType<typeof lancedb.connect>>["openTable"]>> | undefined;
	private vectorIndexBuild: Promise<void> | undefined;
	private readonly databasePath: string;

	constructor(databasePath: string) {
		this.databasePath = databasePath;
	}

	private async getDatabase(): Promise<Awaited<ReturnType<typeof lancedb.connect>>> {
		if (!this.database) {
			await mkdir(this.databasePath, { recursive: true, mode: 0o700 });
			this.database = await lancedb.connect(this.databasePath);
		}
		return this.database;
	}

	private async getTable(seed?: readonly StoredSearchDocument[]): Promise<NonNullable<typeof this.table> | undefined> {
		if (this.table) return this.table;
		const database = await this.getDatabase();
		try {
			this.table = await database.openTable(TABLE_NAME);
			return this.table;
		} catch {
			if (!seed || seed.length === 0) return undefined;
			this.table = await database.createTable(TABLE_NAME, [...seed] as unknown as Record<string, unknown>[]);
			await this.ensureIndexes(this.table);
			return this.table;
		}
	}

	private async getStateTable(): Promise<NonNullable<typeof this.stateTable>> {
		if (this.stateTable) return this.stateTable;
		const database = await this.getDatabase();
		try {
			this.stateTable = await database.openTable(STATE_TABLE_NAME);
		} catch {
			this.stateTable = await database.createTable(STATE_TABLE_NAME, [
				{
					key: "session-search",
					indexVersion: SEARCH_INDEX_VERSION,
					vectorIndex: "flat",
					updatedAt: Date.now(),
				} satisfies SearchStateRow,
			]);
		}
		return this.stateTable;
	}

	private async writeState(vectorIndex: SearchStateRow["vectorIndex"]): Promise<void> {
		const table = await this.getStateTable();
		await table.delete("key = 'session-search'");
		await table.add([
			{
				key: "session-search",
				indexVersion: SEARCH_INDEX_VERSION,
				vectorIndex,
				updatedAt: Date.now(),
			} satisfies SearchStateRow,
		]);
	}

	private async ensureIndexes(table: NonNullable<typeof this.table>): Promise<void> {
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
				// Existing indexes and short-lived concurrent initializers are both harmless.
			}
		}
	}

	async replaceSessionDocuments(sessionPath: string, documents: readonly SearchDocument[]): Promise<void> {
		const existingTable = await this.getTable();
		const existingRows = existingTable
			? ((await existingTable
					.query()
					.where(`sessionPath = ${quotePredicate(sessionPath)}`)
					.toArray()) as LanceSearchRow[])
			: [];
		const existingById = new Map(
			existingRows.flatMap((row) => {
				const documentId = asString(row.documentId);
				const contentHash = asString(row.contentHash);
				const embedding = asEmbedding(row.embedding);
				return documentId && contentHash && embedding
					? [[documentId, { contentHash, embedding, embeddingReady: row.embeddingReady === true }] as const]
					: [];
			}),
		);
		const rows: StoredSearchDocument[] = documents.map((document) => {
			const existing = existingById.get(document.documentId);
			return {
				...document,
				embedding:
					existing?.contentHash === document.contentHash
						? existing.embedding
						: new Array(VECTOR_DIMENSIONS).fill(0),
				embeddingReady: existing?.contentHash === document.contentHash ? existing.embeddingReady : false,
			};
		});
		const table = existingTable ?? (await this.getTable(rows));
		if (!table) return;
		await table.delete(`sessionPath = ${quotePredicate(sessionPath)}`);
		if (rows.length > 0) await table.add(rows as unknown as Record<string, unknown>[]);
	}

	async removeSessionsExcept(sessionPaths: readonly string[]): Promise<void> {
		const table = await this.getTable();
		if (!table) return;
		if (sessionPaths.length === 0) {
			await table.delete("true");
			return;
		}
		await table.delete(`sessionPath NOT IN (${sessionPaths.map(quotePredicate).join(", ")})`);
	}

	async removeSession(sessionPath: string): Promise<void> {
		const table = await this.getTable();
		if (table) await table.delete(`sessionPath = ${quotePredicate(sessionPath)}`);
	}

	async initializeState(): Promise<void> {
		await this.getStateTable();
	}

	async searchLexical(query: string, limit: number): Promise<SessionSearchResult[]> {
		const table = await this.getTable();
		if (!table) return [];
		const normalized = normalizeSearchTerms(query);
		if (!normalized) return [];
		const codeLike = isCodeLikeSearchQuery(query);
		const ftsQuery = new lancedb.MultiMatchQuery(normalized, ["titleText", "bodyText", "referenceText"], {
			boosts: codeLike ? [8, 3, 9] : [9, 4, 5],
			operator: lancedb.Operator.And,
		});
		const rows = (await table
			.search(ftsQuery)
			.limit(limit * 3)
			.toArray()) as LanceSearchRow[];
		return this.aggregateRows(rows, "lexical", limit);
	}

	async searchHybrid(query: string, vector: Float32Array, limit: number): Promise<SessionSearchResult[]> {
		const table = await this.getTable();
		if (!table) return [];
		const normalized = normalizeSearchTerms(query);
		if (!normalized) return [];
		const codeLike = isCodeLikeSearchQuery(query);
		const ftsQuery = new lancedb.MultiMatchQuery(normalized, ["titleText", "bodyText", "referenceText"], {
			boosts: codeLike ? [8, 3, 9] : [9, 4, 5],
			operator: lancedb.Operator.And,
		});
		const [ftsBatch, vectorBatch] = await Promise.all([
			firstRecordBatch(
				table
					.search(ftsQuery)
					.withRowId()
					.limit(limit * 3),
			),
			firstRecordBatch(
				table
					.vectorSearch(vector)
					.where("embeddingReady = true")
					.distanceType("cosine")
					.withRowId()
					.limit(limit * 3),
			),
		]);
		if (!ftsBatch && !vectorBatch) return [];
		if (!ftsBatch) return this.aggregateRows((vectorBatch?.toArray() ?? []) as LanceSearchRow[], "semantic", limit);
		if (!vectorBatch) return this.aggregateRows((ftsBatch.toArray() ?? []) as LanceSearchRow[], "lexical", limit);
		const reranker = await lancedb.rerankers.RRFReranker.create();
		const rows = (await reranker.rerankHybrid(query, vectorBatch, ftsBatch)).toArray() as LanceSearchRow[];
		return this.aggregateRows(rows, "hybrid", limit);
	}

	async pendingSemanticDocuments(limit = 256): Promise<SearchDocument[]> {
		const table = await this.getTable();
		if (!table) return [];
		const rows = (await table.query().where("embeddingReady = false").limit(limit).toArray()) as LanceSearchRow[];
		return rows.flatMap((row) => {
			const documentId = asString(row.documentId);
			const sessionPath = asString(row.sessionPath);
			const kind = asKind(row.kind);
			const titleText = asString(row.titleText);
			const bodyText = asString(row.bodyText);
			const referenceText = asString(row.referenceText);
			const displayText = asString(row.displayText);
			const semanticText = asString(row.semanticText);
			if (
				!documentId ||
				!sessionPath ||
				!kind ||
				titleText === undefined ||
				bodyText === undefined ||
				referenceText === undefined ||
				!displayText ||
				!semanticText
			)
				return [];
			return [
				{
					documentId,
					sessionPath,
					kind,
					titleText,
					bodyText,
					referenceText,
					displayText,
					semanticText,
					contentHash: "",
					updatedAt: 0,
				},
			];
		});
	}

	async updateEmbeddings(entries: readonly { documentId: string; vector: Float32Array }[]): Promise<void> {
		const table = await this.getTable();
		if (!table) return;
		for (const entry of entries) {
			await table.update({
				where: `documentId = ${quotePredicate(entry.documentId)}`,
				values: { embedding: [...entry.vector], embeddingReady: true },
			});
		}
		void this.ensureVectorIndex();
	}

	async searchSemantic(vector: Float32Array, limit: number): Promise<SessionSearchResult[]> {
		const table = await this.getTable();
		if (!table) return [];
		const rows = (await table
			.vectorSearch(vector)
			.where("embeddingReady = true")
			.distanceType("cosine")
			.limit(limit * 3)
			.toArray()) as LanceSearchRow[];
		return this.aggregateRows(rows, "semantic", limit);
	}

	async close(): Promise<void> {
		this.table?.close();
		this.table = undefined;
		this.stateTable?.close();
		this.stateTable = undefined;
		this.database = undefined;
	}

	async reset(): Promise<void> {
		await this.close();
		await rm(this.databasePath, { recursive: true, force: true });
	}

	private async ensureVectorIndex(): Promise<void> {
		if (this.vectorIndexBuild) return await this.vectorIndexBuild;
		this.vectorIndexBuild = (async () => {
			const table = await this.getTable();
			if (!table || (await table.countRows("embeddingReady = true")) < ANN_INDEX_THRESHOLD) return;
			const indexes = await table.listIndices();
			if (indexes.some((index) => index.name === VECTOR_INDEX_NAME || index.columns.includes("embedding"))) {
				await this.writeState("hnsw-sq");
				return;
			}
			await table.createIndex("embedding", {
				name: VECTOR_INDEX_NAME,
				config: lancedb.Index.hnswSq({ distanceType: "cosine", numPartitions: 1, m: 16, efConstruction: 200 }),
			});
			await this.writeState("hnsw-sq");
		})().finally(() => {
			this.vectorIndexBuild = undefined;
		});
		return await this.vectorIndexBuild;
	}

	private aggregateRows(
		rows: readonly LanceSearchRow[],
		mode: "lexical" | "semantic" | "hybrid",
		limit: number,
	): SessionSearchResult[] {
		const bySession = new Map<string, SessionSearchResult>();
		for (const row of rows) {
			const sessionPath = asString(row.sessionPath);
			const kind = asKind(row.kind);
			if (!sessionPath || !kind) continue;
			const evidenceKind: SessionSearchEvidenceKind = mode === "semantic" ? "semantic" : kind;
			const source =
				kind === "title"
					? asString(row.displayText)
					: kind === "reference"
						? asString(row.displayText)
						: asString(row.displayText);
			if (!source) continue;
			const rank =
				mode === "semantic"
					? 1 - (asNumber(row._distance) ?? 1)
					: mode === "hybrid"
						? (asNumber((row as LanceSearchRow & { _relevance_score?: unknown })._relevance_score) ?? 0)
						: (asNumber(row._score) ?? 0);
			// LanceDB's RRF score is already the final cross-source rank. Applying
			// presentation boosts here would incorrectly let a weak title outrank a
			// stronger semantic discussion.
			const score = mode === "hybrid" ? rank : rank + scoreForKind(evidenceKind);
			const candidate: SessionSearchResult = {
				sessionPath,
				score,
				evidence: {
					kind: evidenceKind,
					label: labelForKind(evidenceKind),
					snippet: normalizeSearchPreview(source, 180),
				},
			};
			const existing = bySession.get(sessionPath);
			if (!existing || candidate.score > existing.score) bySession.set(sessionPath, candidate);
		}
		return [...bySession.values()].sort((left, right) => right.score - left.score).slice(0, limit);
	}
}

export class SessionSearchService {
	private readonly repository: LanceSessionSearchRepository;
	private reconciled = false;
	private readonly embeddingWorker: LocalEmbeddingEngine;
	private readonly onSemanticStatus: ((status: LocalEmbeddingStatus | undefined) => void) | undefined;
	private readonly indexedSessionFingerprints = new Map<string, string>();

	constructor(options: {
		agentDir: string;
		cwd: string;
		embeddingEngine?: LocalEmbeddingEngine;
		onSemanticStatus?: (status: LocalEmbeddingStatus | undefined) => void;
	}) {
		const workspaceKey = stableHash(options.cwd);
		this.repository = new LanceSessionSearchRepository(
			join(options.agentDir, "search", SEARCH_INDEX_VERSION, workspaceKey),
		);
		// Avoid loading the model until a semantic query actually needs it.
		this.onSemanticStatus = options.onSemanticStatus;
		this.embeddingWorker =
			options.embeddingEngine ??
			new LocalEmbeddingWorker(options.agentDir, { onStatus: (status) => this.onSemanticStatus?.(status) });
	}

	async reconcile(sessions: readonly SessionInfo[]): Promise<void> {
		try {
			await this.reconcileOnce(sessions);
		} catch {
			// The index is derived. Rebuild a damaged Lance directory rather than
			// letting it block a JSONL-backed conversation forever.
			await this.repository.reset();
			this.indexedSessionFingerprints.clear();
			await this.reconcileOnce(sessions);
		}
	}

	async search(query: string, sessions: readonly SessionInfo[], limit = 30): Promise<SessionSearchResult[]> {
		if (!query.trim()) return [];
		if (!this.reconciled) await this.reconcile(sessions);
		const indexed = await this.repository.searchLexical(query, limit);
		return this.mergeLiveOverlayResults(query, sessions, indexed, limit);
	}

	async searchSemantic(query: string, sessions: readonly SessionInfo[], limit = 30): Promise<SessionSearchResult[]> {
		if (!query.trim()) return [];
		if (!this.reconciled) await this.reconcile(sessions);
		const pending = await this.repository.pendingSemanticDocuments();
		if (pending.length > 0) {
			const vectors = await this.embeddingWorker.embedDocuments(pending.map((document) => document.semanticText));
			await this.repository.updateEmbeddings(
				pending.flatMap((document, index) => {
					const vector = vectors[index];
					return vector ? [{ documentId: document.documentId, vector }] : [];
				}),
			);
		}
		const indexed = await this.repository.searchHybrid(query, await this.embeddingWorker.embedQuery(query), limit);
		this.onSemanticStatus?.(undefined);
		return this.mergeLiveOverlayResults(query, sessions, indexed, limit);
	}

	invalidate(): void {
		this.reconciled = false;
	}

	async remove(sessionPath: string): Promise<void> {
		await this.repository.removeSession(sessionPath);
		this.indexedSessionFingerprints.delete(sessionPath);
	}

	async dispose(): Promise<void> {
		await this.embeddingWorker.dispose();
		await this.repository.close();
	}

	private async reconcileOnce(sessions: readonly SessionInfo[]): Promise<void> {
		const persistedSessions = sessions.filter(
			(session) => session.path.endsWith(".jsonl") && existsSync(session.path),
		);
		await this.repository.initializeState();
		for (const session of persistedSessions) {
			const fingerprint = `${session.modified.getTime()}:${session.messageCount}:${session.name ?? ""}`;
			if (this.indexedSessionFingerprints.get(session.path) === fingerprint) continue;
			const documents = await extractSessionSearchDocuments(session);
			await this.repository.replaceSessionDocuments(session.path, documents);
			this.indexedSessionFingerprints.set(session.path, fingerprint);
		}
		await this.repository.removeSessionsExcept(persistedSessions.map((session) => session.path));
		const persistedPaths = new Set(persistedSessions.map((session) => session.path));
		for (const sessionPath of this.indexedSessionFingerprints.keys()) {
			if (!persistedPaths.has(sessionPath)) this.indexedSessionFingerprints.delete(sessionPath);
		}
		this.reconciled = true;
	}

	private mergeLiveOverlayResults(
		query: string,
		sessions: readonly SessionInfo[],
		indexed: readonly SessionSearchResult[],
		limit: number,
	): SessionSearchResult[] {
		const results = new Map(indexed.map((result) => [result.sessionPath, result]));
		const queryTerms = normalizeSearchTerms(query).split(" ").filter(Boolean);
		for (const session of sessions) {
			if (results.has(session.path) || existsSync(session.path)) continue;
			const title = session.name || session.firstMessage;
			const titleTerms = normalizeSearchTerms(title);
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
}

export function getSessionSearchDatabasePath(agentDir: string, cwd: string): string {
	return join(agentDir, "search", SEARCH_INDEX_VERSION, stableHash(cwd));
}
