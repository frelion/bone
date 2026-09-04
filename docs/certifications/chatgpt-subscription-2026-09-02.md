# ChatGPT subscription certification — 2026-09-02

This record contains no credential, account, device-code, or provider-response
content.

## Build

- BONE branch: `main` working tree prepared after `132b12b8`
- Provider: `bone-provider 0.1.0`
- Rig: repository-patched `rig-core 0.42.0`
- Target: Linux x86_64
- Model requested: `gpt-5.4`
- Credential source: BONE's independent cached ChatGPT subscription login

## Command

```sh
BONE_CHATGPT_MODEL=gpt-5.4 \
  cargo test -p bone-provider --test live_chatgpt_subscription \
  --locked -- --ignored --nocapture
```

The crate was subsequently renamed to `bone-llm`; the command above records
the package name used by this historical run.

## Result

- Explicit managed connection and cached authorization: pass
- Responses streaming text with exact `ok`: pass
- One terminal record with resolved model and nonzero usage: pass
- Forced `inspect_path` tool call: pass
- Tool name, arguments, and provider call ID preservation: pass
- Tool-result replay with exact final `done`: pass
- Test result: `1 passed; 0 failed` in 9.04 seconds

The fictional tool was not executed and did not access the filesystem.

## Deliberately not exercised

- A forced token expiry/refresh was not manufactured because the OAuth
  endpoint is fixed inside Rig and credential contents were not inspected or
  modified. Refresh generation, retry bounds, invalidation, and corrupt-cache
  recovery are covered by the pinned Rig's offline tests.
- `disconnect` followed by a fresh device login was not forced in this run,
  because it would deliberately invalidate the working local login and require
  synchronous user authorization. Store deletion, lock lifetime, and clean
  reconnect prerequisites are covered offline.
