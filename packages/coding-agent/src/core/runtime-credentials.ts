import type { Credential, CredentialInfo, CredentialStore } from "@frelion/bone-ai";

/** Async credential store overlay for non-persistent runtime API keys. */
export class RuntimeCredentials implements CredentialStore {
	private readonly store: CredentialStore;
	private readonly overrides = new Map<string, Credential>();

	constructor(store: CredentialStore) {
		this.store = store;
	}

	setRuntimeApiKey(providerId: string, apiKey: string): void {
		this.setRuntimeCredential(providerId, { type: "api_key", key: apiKey });
	}

	/** Apply a credential to this runtime only; it is never persisted by this store. */
	setRuntimeCredential(providerId: string, credential: Credential): void {
		this.overrides.set(providerId, structuredClone(credential));
	}

	removeRuntimeApiKey(providerId: string): void {
		this.removeRuntimeCredential(providerId);
	}

	removeRuntimeCredential(providerId: string): void {
		this.overrides.delete(providerId);
	}

	hasRuntimeApiKey(providerId: string): boolean {
		return this.overrides.has(providerId);
	}

	async read(providerId: string): Promise<Credential | undefined> {
		const override = this.overrides.get(providerId);
		return override ? structuredClone(override) : this.store.read(providerId);
	}

	async list(): Promise<readonly CredentialInfo[]> {
		const entries = new Map((await this.store.list()).map((entry) => [entry.providerId, entry]));
		for (const [providerId, credential] of this.overrides) {
			entries.set(providerId, { providerId, type: credential.type });
		}
		return [...entries.values()];
	}

	modify(
		providerId: string,
		fn: (current: Credential | undefined) => Promise<Credential | undefined>,
	): Promise<Credential | undefined> {
		return this.store.modify(providerId, fn);
	}

	/** Replace the persistent credential for one Provider without creating a session override. */
	async setStoredCredential(providerId: string, credential: Credential): Promise<void> {
		await this.store.modify(providerId, async () => structuredClone(credential));
		this.overrides.delete(providerId);
	}

	async delete(providerId: string): Promise<void> {
		this.overrides.delete(providerId);
		await this.store.delete(providerId);
	}

	/** Refresh file-backed auth.json stores after another live runtime saved credentials. */
	reloadStoredCredentials(): void {
		const reloadable = this.store as CredentialStore & { reload?: () => void };
		reloadable.reload?.();
	}
}
