const CJK_CHARACTER = /[\u3400-\u9fff\uf900-\ufaff]/u;
const PATH_OR_IDENTIFIER = /[A-Za-z0-9_./-]+/g;
const CAMEL_CASE_BOUNDARY = /([a-z\d])([A-Z])/g;
const ACRONYM_BOUNDARY = /([A-Z]+)([A-Z][a-z])/g;

function addToken(tokens: Set<string>, value: string): void {
	const token = value.trim().toLocaleLowerCase();
	if (token) tokens.add(token);
}

function addIdentifierTokens(tokens: Set<string>, value: string): void {
	addToken(tokens, value);
	const expanded = value.replace(ACRONYM_BOUNDARY, "$1 $2").replace(CAMEL_CASE_BOUNDARY, "$1 $2");
	for (const part of expanded.split(/[\s_./-]+/)) addToken(tokens, part);
}

function addCjkBigrams(tokens: Set<string>, value: string): void {
	const characters = [...value].filter((character) => CJK_CHARACTER.test(character));
	for (let index = 0; index < characters.length - 1; index++) {
		addToken(tokens, `${characters[index]}${characters[index + 1]}`);
	}
}

/** Normalize natural language, paths, and identifiers into deterministic FTS terms. */
export function normalizeSearchTerms(value: string): string {
	const normalized = value.normalize("NFKC").replace(/[\r\n\t]+/g, " ");
	const tokens = new Set<string>();
	addCjkBigrams(tokens, normalized);

	for (const match of normalized.matchAll(PATH_OR_IDENTIFIER)) {
		addIdentifierTokens(tokens, match[0]);
	}

	return [...tokens].join(" ");
}

export function normalizeSearchPreview(value: string, maxLength = 360): string {
	const compact = value
		.replace(/[\r\n\t]+/g, " ")
		.replace(/\s+/g, " ")
		.trim();
	return compact.length <= maxLength ? compact : `${compact.slice(0, Math.max(1, maxLength - 1))}…`;
}

export function isCodeLikeSearchQuery(query: string): boolean {
	return /[./\\_]|[a-z][A-Z]|\.[A-Za-z0-9]+$/.test(query);
}
