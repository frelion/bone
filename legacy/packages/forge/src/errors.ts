export type ForgeErrorCode =
	| "authentication_required"
	| "permission_denied"
	| "not_found"
	| "validation_failed"
	| "conflict"
	| "rate_limited"
	| "unsupported_capability"
	| "policy_denied"
	| "approval_required"
	| "ambiguous_result"
	| "remote_failure"
	| "invalid_remote_response"
	| "unsafe_remote";

export interface ForgeErrorDetails {
	status?: number;
	provider?: "gitlab" | "github";
	host?: string;
	operation?: string;
	retryAfterSeconds?: number;
	capability?: string;
	[key: string]: unknown;
}

export class ForgeError extends Error {
	readonly code: ForgeErrorCode;
	readonly details: ForgeErrorDetails;

	constructor(code: ForgeErrorCode, message: string, details: ForgeErrorDetails = {}, options?: ErrorOptions) {
		super(message, options);
		this.name = "ForgeError";
		this.code = code;
		this.details = details;
	}
}

export function redactSecrets(value: string, secrets: readonly string[]): string {
	let redacted = value;
	for (const secret of secrets) {
		if (secret.length > 0) redacted = redacted.split(secret).join("[REDACTED]");
	}
	redacted = redacted.replace(/(private-token|authorization)(\s*[:=]\s*)([^\s,;]+)/gi, "$1$2[REDACTED]");
	redacted = redacted.replace(/([?&](?:private_token|access_token)=)[^&\s]+/gi, "$1[REDACTED]");
	return redacted;
}
