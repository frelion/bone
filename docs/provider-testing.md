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

## Product composition test

`bone-app` is headless and currently has no binary or TUI. Its deterministic
tests inject a `ModelPort` behind the crate-private composition seam and drive
the same public `App` and `Session` API that a future frontend will use:

```sh
cargo test -p bone-app --all-targets --locked
```

These tests cover typed configuration, lazy Runtime startup, durable input and
history, provider connection ownership, login-required state, shutdown,
external-write accounting, and restart boundaries. See
[configuration and storage](configuration.md) for the public setup contract.

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

```

ChatGPT subscription protocol behavior is covered by offline contract fixtures.
The App additionally tests cached-auth connection ownership without making a
network request. Interactive device authorization belongs to an explicit host
calling `App::login`; there is no ignored live App test in the current tree.
Device codes, OAuth payloads, and refresh tokens must never be copied to a
fixture, test output, certification record, debug log, or model-visible
message.

`.github/workflows/provider-live.yml` is manual-only. It reads API keys from
GitHub Actions secrets and model/base-URL settings from repository variables;
ordinary pushes and pull requests do not trigger paid requests.

Live tests assert structural behavior rather than exact prose: non-empty text,
one terminal event, a provider-resolved model identity, and a complete
response.

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
