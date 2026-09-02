import type { TextContent, TextPhase, TextSignatureV1 } from "../types.ts";

export interface ParsedTextSignature {
	id: string;
	phase?: TextPhase;
}

export function isTextPhase(value: unknown): value is TextPhase {
	return value === "commentary" || value === "final_answer";
}

export function encodeTextSignature(id: string, phase?: TextPhase): string {
	const payload: TextSignatureV1 = { v: 1, id };
	if (phase) payload.phase = phase;
	return JSON.stringify(payload);
}

export function parseTextSignature(signature: string | undefined): ParsedTextSignature | undefined {
	if (!signature) return undefined;
	if (signature.startsWith("{")) {
		try {
			const parsed = JSON.parse(signature) as Partial<TextSignatureV1>;
			if (parsed.v === 1 && typeof parsed.id === "string") {
				return {
					id: parsed.id,
					...(isTextPhase(parsed.phase) ? { phase: parsed.phase } : {}),
				};
			}
		} catch {
			// Preserve malformed and legacy signatures as opaque provider IDs.
		}
	}
	return { id: signature };
}

export function getTextContentPhase(content: Pick<TextContent, "textSignature">): TextPhase | undefined {
	return parseTextSignature(content.textSignature)?.phase;
}
