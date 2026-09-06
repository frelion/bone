# Provider protocol testing

Provider tests prove BONE's declared boundary, rather than retesting every Rig
implementation detail. The default test suite is deterministic, offline, and
does not require credentials.

## Test layers

| Layer | Location | Contract |
| --- | --- | --- |
| Core identity | `tests/model_contract.rs` and source unit tests | Endpoint, protocol, and model identities remain separate; credentials are not rendered |
| OpenAI wire | `tests/openai_responses_contract.rs` | `POST /responses`, headers, text, tools, reasoning replay, SSE terminal/truncation, usage, error body |
| OpenAI Chat wire | `tests/openai_chat_completions_contract.rs` and source unit tests | `POST /chat/completions`, headers, text, tools, SSE terminal/truncation, usage, error body |
| Anthropic wire | `tests/anthropic_messages_contract.rs` | `POST /v1/messages`, headers, text, tools, caching, SSE terminal/truncation, usage, error body |
| ChatGPT subscription service | `tests/chatgpt_subscription_contract.rs` | Default and custom Codex Responses URLs, headers, forced SSE/body rules, text, tools, replay, and identity |
| Live certification | `tests/live_*.rs` | A configured real endpoint currently accepts the declared protocol |

Fixtures live below `tests/fixtures/<protocol>/`. Request bodies are parsed as
JSON and compared semantically; tests do not depend on object key order. The
small transport in `tests/support/transport.rs` composes Rig's official test
doubles and records only the request metadata Rig does not retain itself. It
is never compiled into the production API.

Run the offline checks with:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo test --workspace --doc --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo check -p bone-llm --lib --target wasm32-unknown-unknown --locked
```

The root CI workflow runs those commands on Linux and also compiles the
workspace on Windows. Ignored live tests are built by `--all-targets` but are
not executed. The WASM check compiles only the model library; native
file-backed configuration and subscription OAuth remain unavailable on that
target.

## Agent frontend smoke test

Configure `agent.system` as described in [Configuration](configuration.md),
then run the terminal frontend. `bone-agent` constructs the subscription
models, tools, and runtime from a shared configuration snapshot:

```sh
cargo run -p bone-tui -- "Reply with exactly: ok"

cargo run -p bone-tui
```

In the full-screen form, use `Ctrl-N` to create a conversation,
`Alt-Up`/`Alt-Down` to switch, `Esc` to stop the current session, and `Ctrl-C`
to exit. The first run may require device authorization.
`llm.system.credential_root` can select an independent credential directory;
omitting it retains BONE's conventional directory.

## Live certification

Live tests are ignored by default because they require network access, secret
credentials, and may incur billing. Run them deliberately:

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_MODEL='...'
# Optional:
export OPENAI_BASE_URL='https://gateway.example/v1'
cargo test -p bone-llm --test live_openai_responses -- --ignored --nocapture

export OPENAI_API_KEY='...'
export BONE_OPENAI_CHAT_MODEL='...'
# Optional:
export OPENAI_BASE_URL='https://gateway.example/v1'
cargo test -p bone-llm --test live_openai_chat_completions -- --ignored --nocapture

export ANTHROPIC_API_KEY='...'
export BONE_ANTHROPIC_MODEL='...'
# Optional:
export ANTHROPIC_BASE_URL='https://gateway.example'
cargo test -p bone-llm --test live_anthropic_messages -- --ignored --nocapture

export BONE_CHATGPT_MODEL='a-model-available-to-your-subscription'
cargo test -p bone-llm \
  --test live_chatgpt_subscription -- --ignored --nocapture

BONE_CONFIG='/absolute/path/config.json' \
  cargo test -p bone-agent --test live_agent -- --ignored --nocapture
```

`.github/workflows/provider-live.yml` is manual-only. It reads API keys from
GitHub Actions secrets and model/base-URL settings from repository variables;
ordinary pushes and pull requests can never trigger paid requests.

The ChatGPT subscription certification is intentionally local-only. Its first
run may require interactive device authorization and subsequent runs use an
independent local OAuth cache. Never upload a personal ChatGPT refresh token to
GitHub-hosted Actions. If an organization later automates this test, it should
use a dedicated account or supported access token on a trusted private runner.
Non-sensitive results from deliberate local runs are recorded under
`docs/certifications/`; credential and provider-response content must never be
included in those records.

The workflow fails before checkout when no protocol has a complete
credential/model selection, or when a selected model lacks its credential.
Likewise, a configured credential with no model fails instead of producing a
fully skipped successful run.

Responses and Chat Completions share `OPENAI_API_KEY` and `OPENAI_BASE_URL`
because one workflow run certifies one OpenAI-compatible service. They use
independent `BONE_OPENAI_MODEL` and `BONE_OPENAI_CHAT_MODEL` repository
variables. Configure either variable to certify only that protocol, or both to
prove the same service supports both paths. A service can legitimately support
only one. Services with different URLs or credentials should be certified in
separate manual runs with the repository configuration switched between runs
(or a future environment matrix), rather than conflated into one endpoint
identity.

Most live tests assert structural behavior rather than exact prose: non-empty
text, one real terminal event, a non-empty provider-resolved model identity,
and a complete response. They do not require a provider to echo a requested
alias verbatim. The ChatGPT subscription certification deliberately asks for
and asserts the exact text `ok`, then forces a harmless fictional tool call,
replays the result with the provider call id, and asserts the exact final text
`done`. The tool is not executed and does not access the filesystem. Output
limits stay low to bound cost.

## Adding coverage

For another endpoint speaking an existing protocol:

1. Run the existing protocol's live test with that endpoint's URL, key, and
   model.
2. If it passes, add only runtime configuration or a CI matrix entry.
3. If it exposes a stable compatibility difference, first add an offline
   fixture reproducing the difference, then add the smallest typed option to
   that protocol module.

For a genuinely new protocol:

1. Add an explicit `Protocol` variant and one protocol module.
2. Add official and compatible constructors returning `Endpoint`.
3. Cover authentication, URL normalization, request JSON, unary response,
   stream termination, tools, usage, and error preservation offline.
4. Add an ignored live certification target only after the offline contract
   is complete.

Never put real credentials in fixtures, snapshots, error assertions, or test
output, and never embed credentials in a configured base URL. The opt-in
`test-utils` feature exposes hidden constructors for this crate's offline
contract tests; production callers never receive a Rig client or transport.
Those tests use static fake credentials and recording transports and never read
the managed OAuth cache. Authentication 401/403 and Responses SSE provider
error envelopes are redacted by the pinned Rig patch. Live-test failures must
still not be copied into public logs without review.
