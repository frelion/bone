# Rig 0.42.0 ChatGPT hardening

This repository binds its `rig-core` workspace dependency directly to the
self-contained copy in `third_party/rig-core-0.42.0`. The copy starts from the
published 0.42.0 crate and retains its upstream `LICENSE`, `README.md`, and
`Cargo.toml.orig`.

`bone-model` is intentionally marked `publish = false` while this direct
path dependency is required. Git and path consumers keep the vendored
dependency, but a crates.io package cannot include a dependency outside its
package directory. Remove that guard only after depending on a published Rig
(or a published replacement package) with equivalent fixes.

Upstream baseline:

- package: `rig-core = 0.42.0`
- repository: `https://github.com/0xPlaygrounds/rig`
- release tag commit: `d5a34986a1ad57f1e9c5984b82f8d7438ffc717e`

The local delta is deliberately limited to these files:

- `src/providers/internal/device_auth.rs`
- `src/providers/chatgpt/auth/mod.rs`
- `src/providers/chatgpt/auth/native.rs`
- `src/providers/chatgpt/auth/wasm.rs`
- `src/providers/chatgpt/mod.rs`

The delta provides:

- atomic Unix credential replacement using a private sibling file, file and
  directory sync, and same-directory rename;
- corrupt-cache invalidation, with recovery reserved for explicit interactive
  authorization;
- a single-flight refresh after a rejected access-token generation, followed
  by at most one unary or streaming request retry;
- invalidation after an unusable refresh token;
- stable redaction for unary/streaming 401 or 403 responses and Responses SSE
  provider-error envelopes, while retaining non-authentication transport and
  structured parse errors.

The non-Unix writer intentionally retains Rig's original replacement behavior.
BONE's managed ChatGPT subscription connector is Unix-only, so this is not a
weaker path for the supported integration. Do not claim atomic credential
writes on another platform until Rig gains a platform-appropriate atomic
replacement implementation there.

Before removing the vendored path dependency during a Rig upgrade, verify that
the upstream release contains equivalent regression tests for atomic writes,
corrupt cache recovery, rejected-token generation handling, retry bounds, and
unary plus SSE error redaction.
