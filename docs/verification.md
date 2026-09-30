# Rebuild verification

The acceptance suite uses a local scripted OpenAI Responses HTTP server and the
real runtime. It supplies only a dummy key and writes all state to temporary
directories. It does not read personal configuration or make real model calls.
The server records request bodies, never authorization headers.

## Scenario fixtures

`fixtures/acceptance/cases.json` describes six scenarios:

| Scenario | External verification |
| --- | --- |
| Small repair | Empty and mixed signed totals, including `None` |
| Independent tasks | Correct slug conversion and inclusive clamp behavior |
| Sequential dependency | Producer JSON Schema and matching consumer validation |
| Followup and constraint | Same session, revised greeting and allowed file set |
| Long context | Three separated rules in 3,600 lines and unchanged source |
| Recovery | Separate CLI processes reuse one session; idempotent ledger |

Each trial starts from a clean temporary workspace. Verifiers run outside that
workspace. A successful CLI exit alone never counts as passing. The recovery
fixture tests process restart; interrupted effects and uncertain writes require
the separately scripted runtime tests.

## Alternating ablations

Print the schedule without making requests:

```sh
python3 scripts/ablate.py
```

Run with an explicitly selected model profile and data directory:

```sh
python3 scripts/ablate.py --run --profile experiment \
  --data-dir /absolute/path/to/experiment-data \
  --output /absolute/path/to/results.jsonl
```

The default schedule runs each pair in AB, BA, AB order with three repetitions:
multiple jobs versus `--single-job`, parallel execution versus
`--max-parallel 1`, and compaction versus `--no-compaction`. Profile, fixture,
parallel limit and timeout remain fixed across each comparison. All six cases
and three pairs produce 108 trials. Use `--case independent_tasks --pair jobs`
to select a comparison, or `--baseline-only` for one default run of each case.
`--max-calls` and `--context-chars` fix the request and context budgets.

Every trial, including failures, is flushed to JSONL. Records include exit
status, external checks, model calls, input/output/total token usage, wall time,
and provider-reported dollar cost. Unreported values remain `null`; missing
metrics are never treated as zero. Output files are created exclusively so a
rerun cannot silently replace an earlier experiment. With `--artifacts-dir`,
each trial retains its workspace, raw CLI stdout/stderr, and event history.
Credential contents are never read by the harness. The configuration checksum
and binary checksum identify the experiment; use a copied binary to hold its
code fixed while development continues.

The configuration checksum is recorded and checked before every trial. A
configuration change marks subsequent trials failed without making requests.
An inherited `BONE_MODEL` override is removed so it cannot bypass the chosen
profile. Followup phases use the actual returned session ID, and each phase
starts a separate CLI process.

The script executes model requests only with `--run`; real provider runs may be
billable. A scripted-server pass demonstrates protocol and runtime behavior,
not model quality or an efficiency gain. Efficiency conclusions require the
actual recorded paired runs, including their failures.

## Validation status

Observed offline validation:

- Final macOS checks passed: `cargo fmt --check`, Clippy with warnings denied,
  all 65 Rust tests, and `cargo check --locked --all-features`. The latter
  compiles the official Bedrock, Vertex AI and Candle constructors; those
  deployments and local weights have not received live inference checks.
- `python3 -B -m unittest discover -s tests -p 'test_ablate.py' -v`: 5 checks passed.
- `python3 scripts/ablate.py --case small_repair`: generated the 18 planned
  alternating trials and made no model requests.
- Scripted unary/SSE envelopes checked for separate in-progress and completed
  documents and valid serialized tool arguments.
- `cargo test --test acceptance -- --nocapture`: 9 tests passed. This includes
  real file writes, persisted call ownership and usage, Idle continuation across
  process restart, focus handoff, provider failure, exact assignment replies,
  stale write rejection, and interruption/restart/reconciliation without a
  duplicate append.
- A request that fails after an earlier reported usage now leaves total token
  fields `null`; previously observed tokens are not reported as a known total.
- The Rust suite also executed 18 alternating small-repair trials through the
  actual CLI and local Responses server: all passed, with 54 model requests and
  recorded per-trial usage. This validates the measurement harness.

Live subscription acceptance is recorded separately below. The scripted suite
does not establish model quality or coverage of other provider deployments.

## Live subscription baseline (2026-09-30)

The six original fixtures ran once using `chatgpt:gpt-6-luna` and the user's
existing subscription login. The harness did not inspect credentials or login
again. Binary SHA256:
`879bb51568d13d9cb7a7c366c57f9a4e2bdf2244e5880a3e9938e00362e8d00b`.
Budgets were 64 calls, 96,000 serialized context characters, 300 seconds and
three parallel jobs. Every cost field was `null`.

| Fixture | Passed | Calls | Input tokens | Output tokens | Wall seconds |
| --- | --- | --- | --- | --- | --- |
| Small repair | Yes | 6 | 8,128 | 367 | 19.56 |
| Independent tasks | Yes | 16 | 28,720 | 2,131 | 71.70 |
| Sequential dependency | Yes | 8 | 14,850 | 1,385 | 39.68 |
| Followup constraint | Yes | 10 | 15,602 | 572 | 31.48 |
| Long context | Yes | 9 | 75,921 | 833 | 32.94 |
| Recovery | No | 9 | 16,287 | 1,013 | 37.30 |

Recovery's two processes completed on the same session, but the implementation
opened the ledger with `r+`, so a new path raised `FileNotFoundError`. The external
verifier caught that defect. An explicit corrective followup in the same session
passed independently: four more calls, 12,760 input tokens, 527 output tokens,
18.42 seconds. The original recovery baseline remains failed.

The long-context baseline read an initial page and extracted rules with `grep`;
it emitted no summary event. Its pass does not verify compaction. A separate
40,000-character diagnostic pair on this old binary recorded compaction failing
after two calls with `no complete prefix can be safely summarized`, while the
uncompacted run passed using smaller reads. These diagnostic trials are excluded
from the formal comparison after the corresponding context fix.

Raw records and event histories are under
`/var/folders/16/1dvtn08j5d15sssx4qdzy_r80000gn/T/bone-live-acceptance-gjpf26xo`:
`baseline.jsonl`, `recovery-followup-repair/`, `compaction-first.jsonl`, and
`trials/<trial_id>/`. These local temporary artifacts must be retained separately
if they need to survive system cleanup. A separate API-key probe made one
request and failed with `429 credit_balance_exhausted`; it was not retried and
does not establish API-key model access.

Compact evidence retained in the repository:

- [Original six baselines](results/2026-09-30-gpt-6-luna/baseline.jsonl)
- [Explicit recovery correction](results/2026-09-30-gpt-6-luna/recovery-followup-repair.json)
- [Old binary context diagnostic](results/2026-09-30-gpt-6-luna/compaction-first.jsonl)
- [Final binary compaction comparison](results/2026-09-30-gpt-6-luna/final-compaction.jsonl)
- [Final binary jobs and parallel comparisons](results/2026-09-30-gpt-6-luna/final-independent.jsonl)

Older trial event counts were reconstructed from their saved original histories;
their outcomes and usage are unchanged. The compact records contain metrics,
external checks, and event counts, without credentials, binaries, or SQLite.

## Final binary: 18 alternating subscription trials

All 18 formal trials used the copied binary with SHA256
`830b265fe3bd095c45d465aa6356dcd0aa7ffa8282ccc89311e786e30d2ce26f`,
the same subscription profile and fixture manifest, 64 calls maximum and a
300-second timeout. Each pair ran AB, BA, AB. The independent-task comparisons
used 96,000 context characters; the long-context comparison used 40,000 on both
sides. Parallel capacity was three except the serial variant's one.

The table includes every trial, including the three failures. Token and time
columns are means across three runs; cost was unreported (`null`) throughout.

| Comparison / variant | Passed | Mean calls | Mean input tokens | Mean output tokens | Mean wall seconds | Actual Job count |
| --- | --- | --- | --- | --- | --- | --- |
| Jobs / multi job | 3/3 | 16.00 | 45,399.67 | 1,529.00 | 64.84 | 2 |
| Jobs / single job | 3/3 | 6.00 | 8,734.33 | 795.67 | 28.14 | 1 |
| Parallel / parallel | 3/3 | 17.33 | 30,526.33 | 1,804.00 | 70.14 | 2–3 |
| Parallel / serial | 3/3 | 18.33 | 33,266.33 | 2,123.00 | 78.49 | 2–3 |
| Context / compaction | 3/3 | 12.00 | 38,244.33 | 1,037.67 | 47.94 | 1 |
| Context / no compaction | 0/3 | 2.33 | 2,858.67 | 101.33 | 8.75 | 1 |

Overall: 15/18 passed, 216 model calls, 477,089 input tokens, 22,172 output
tokens, 499,261 total tokens, and 894.88 seconds of trial wall time. Model
requests stopped after the planned 18 trials.

Multiple jobs had greater overhead on these small independent functions. The
parallel sample completed sooner on average, but its plans and call counts also
differed; three runs do not establish a general efficiency gain. Compaction
actually emitted one summary in each of repetitions two and three. All three
disabled-compaction runs hit the configured context limit and failed; their
early termination explains their lower usage and time.

The final binary also passed the repository's reported formatting, Clippy with
warnings denied, 65 Rust tests, all-features compile check, build, and five
Python harness tests. Scripted OpenAI Responses acceptance validates runtime
contracts. Real successful calls here validate only the `gpt-6-luna` ChatGPT
subscription path. Other registry providers, companion providers and API-key
access have not passed a real end-to-end model call in this acceptance work.
Compilation and registry contracts do not verify image or audio product flows.

## Chinese conversation on the final binary

Five ordinary Chinese inputs continued one session with the same frozen binary,
`chatgpt:gpt-6-luna`, and read-only reuse of the existing Codex login. All five
passed; no input selected a Job. Each turn had a 16-call, 120-second limit.

| Input | Independently checked outcome |
| --- | --- |
| 先解释 greeting.py 的行为，不修改文件 | Correct explanation; file hash unchanged |
| 改一下：非空名字 strip 后返回 Hello，纯空白 ValueError | Eight Python assertions passed |
| 继续：Hello 改 Welcome，保留校验 | Eight Python assertions passed |
| 停止 | Native `pause_work`; persisted session paused |
| 继续，总结刚才完成了什么，不再修改文件 | Correct continuation; file hash unchanged; session resumed |

Normal tools remained available during the explanation and final summary;
neither invoked `write_file`. All 14 model calls had Job and Call identities.
Total usage was 25,288 input plus 965 output tokens (26,253 total), with
unreported cost. The five CLI runs took approximately 49.06 seconds in total.

The first CLI run completed before the verifier encountered a SQLite read error.
The verifier was repaired and recovered that saved receipt without repeating
the model request. Time spent repairing the verifier is excluded from the CLI
time above. Production source and binary remained unchanged. The full record,
including this harness limitation, is retained in
[chinese-conversation.json](results/2026-09-30-gpt-6-luna/chinese-conversation.json).
