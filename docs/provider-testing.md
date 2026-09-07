# Provider protocol testing

Provider tests prove BONE's declared boundary rather than retesting every Rig
implementation detail. The normal suite is deterministic, offline, and does
not require credentials.

## Test layers

| Layer | Location | Contract |
| --- | --- | --- |
| Core identity | `tests/model_contract.rs` and source unit tests | Endpoint, protocol, and model identities remain separate; credentials are not rendered. |
| OpenAI wire | `tests/openai_responses_contract.rs` | `/responses`, headers, text, tools, reasoning replay, SSE terminal/truncation, usage, error body. |
| OpenAI Chat wire | `tests/openai_chat_completions_contract.rs` | `/chat/completions`, headers, text, tools, SSE terminal/truncation, usage, error body. |
| Anthropic wire | `tests/anthropic_messages_contract.rs` | `/v1/messages`, headers, text, tools, caching, SSE terminal/truncation, usage, error body. |
| ChatGPT subscription | `tests/chatgpt_subscription_contract.rs` | Codex Responses URLs, headers, forced SSE/body rules, text, tools, replay, identity, and redaction. |
| Live certification | `tests/live_*.rs` | A deliberately configured real endpoint accepts its declared protocol. |

Fixtures live below `tests/fixtures/<protocol>/`. Request bodies are parsed as
JSON and compared semantically, so object key order is not a contract. The
small test transport composes Rig's official test doubles and records only
metadata Rig does not retain; it is never compiled into the production API.

Run the local checks with:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --offline -- -D warnings
cargo test --workspace --all-features --offline
```

Ignored live tests are built but not executed by the normal suite.

## Product smoke test

No configuration file is required. Start BONE in a terminal, set a model in
the TUI, then connect when needed:

```sh
cargo run -p bone-app --bin bone

# One-shot form: the model choice is explicit for this invocation.
cargo run -p bone-app --bin bone -- --model gpt-5.6 "Reply with exactly: ok"
```

The interactive first run may require a ChatGPT device authorization. It can
still open a Workspace, Session history, and draft before model selection or
login. See [configuration and storage](configuration.md) for TUI model scopes
and storage repair semantics.

## Live certification

Live tests are ignored by default because they require network access, secrets,
and may incur billing. Run them deliberately:

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_MODEL='...'
# Optional:
export OPENAI_BASE_URL='https://gateway.example/v1'
cargo test -p bone-llm --test live_openai_responses -- --ignored --nocapture

export OPENAI_API_KEY='...'
export BONE_OPENAI_CHAT_MODEL='...'
cargo test -p bone-llm --test live_openai_chat_completions -- --ignored --nocapture

export ANTHROPIC_API_KEY='...'
export BONE_ANTHROPIC_MODEL='...'
cargo test -p bone-llm --test live_anthropic_messages -- --ignored --nocapture

export BONE_CHATGPT_MODEL='a-model-available-to-your-subscription'
cargo test -p bone-llm --test live_chatgpt_subscription -- --ignored --nocapture
```

The ChatGPT subscription certification uses `BoneStore::open_default()` and
acquires `ProviderId::ChatGptSubscription` before it connects. Its first local
run may require device authorization; later local runs reuse Rig's private
cache through the lease. Never upload a personal ChatGPT refresh token to
GitHub-hosted Actions. If an organization automates this test, use a dedicated
account or a supported access token on a trusted private runner.

`/logout` only deletes the local OAuth cache after no Endpoint/Model holds the
lease; it does not revoke an upstream account. Device codes, OAuth payloads,
and refresh tokens must never be copied to a fixture, test output,
certification record, debug log, or model-visible message.

`.github/workflows/provider-live.yml` is manual-only. It reads API keys from
GitHub Actions secrets and model/base-URL settings from repository variables;
ordinary pushes and pull requests do not trigger paid requests.

Most live tests assert structural behavior rather than exact prose: non-empty
text, one terminal event, a provider-resolved model identity, and a complete
response. The ChatGPT subscription certification deliberately asserts `ok`,
then forces a harmless fictional tool call, replays its opaque result, and
asserts `done`. The tool is never executed against the filesystem.

## Adding coverage

For another endpoint speaking an existing protocol:

1. Run that protocol's live test with the endpoint's URL, key, and model.
2. If it passes, add only product composition or CI matrix configuration.
3. If it exposes a stable compatibility difference, first add an offline
   fixture reproducing it, then add the smallest typed protocol option.

For a genuinely new protocol:

1. Add an explicit `Protocol` variant and protocol module.
2. Add official/compatible constructors returning `Endpoint`.
3. Cover authentication, URL normalization, request JSON, unary response,
   stream termination, tools, usage, and error preservation offline.
4. Add an ignored live certification only after the offline contract is
   complete.

Never put real credentials in fixtures, snapshots, error assertions, or output,
and never embed credentials in a configured base URL. The opt-in `test-utils`
feature exposes hidden constructors for offline contracts; production callers
never receive a Rig client or transport.
