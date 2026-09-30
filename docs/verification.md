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

## Long, multi-turn task gate

`fixtures/long_task/task.json` specifies twelve successive inputs for one saved
Session. The task builds a five-file Python ledger: normalized records, exact
integer cents, idempotent writes, batch rollback, a revised UTC timestamp rule,
account queries, stable CSV and a CLI with deferred dry-run support. Explanation
turns require unchanged files. A natural pause is followed by continuation in a
new CLI process. The final verifier stays outside the agent workspace.

```sh
# Inspect the twelve inputs; no requests.
python3 -B scripts/long_task.py

# Use a copied binary and a dedicated data directory with a chosen profile.
python3 -B scripts/long_task.py --run --bone /absolute/path/to/frozen-bone \
  --data-dir /absolute/path/to/data --profile subscription \
  --output-dir /absolute/path/to/new-results
```

Default limits are 48 calls per input, 32,000 serialized context characters,
300 seconds per turn and three parallel actions. The gate requires at least two
actual summary events. It fingerprints the binary, configuration, task and
independent verifier; changing any during the run stops further requests.

Each round retains stdout, stderr, original event history, independent checks
and file-hash comparison outcomes. Model quality failures, execution failures, kernel invariants
and missing coverage are reported separately. A delivered answer alone is not a
passing result. Unknown costs remain `null`. Failed rounds are retained even if
a later turn fixes the code; an explicit correction is reported separately.

`tests/long_session.rs` drives the same public Session API and actual CLI against
a local scripted native provider. It tests repeated compaction and reopening,
exact dependency replies, handoff and changed instructions, natural pause
completion, and a real parent-process hard kill while its shell is still alive.
Private-module fault tests additionally inject transaction failure and populate
large durable transcripts. Their purpose is runtime correctness; they do not
measure real-model task success.

### First live long-session run: a preserved memory failure

The first frozen hardening binary
`1f9b7b54fdf1f1f63ae4c68e8c079554fc34245b79ee752757415f5872ef0633`
ran all twelve inputs with `chatgpt:gpt-6-luna` using the existing Codex login.
The fixed limits above were unchanged. It made 104 model calls, reported 442,990
input and 27,464 output tokens (470,454 total), emitted eleven summaries, and
took 807.72 seconds. Cost was unreported.

The complete gate **failed**. All control outcomes, file checks and final nine
code checks passed, but round six failed the deferred-work requirement: its
answer did not recover the original CSV and dry-run commitments. The first
summary retained both; the second reduced them to generic export/CLI work.
Recovery then traversed mixed audit pages without reaching the original inputs.
Reading summary events returned raw SDK JSON whose first page was occupied by
covered IDs and encrypted reasoning, rather than readable summary text. The
original instructions were still present in SQLite.

The progress display originally printed only the code verifier's boolean as
`checks_passed`; that did not include the deferred-work check. Its final result
correctly marked the run failed. The display was subsequently changed to show
the separate checks and their combined result. The failed measurements and
original answer remain in [trial-1-result.json](results/2026-09-30-long-session/trial-1-result.json),
[trial-1-rounds.jsonl](results/2026-09-30-long-session/trial-1-rounds.jsonl), and
[trial-1-memory-failure.json](results/2026-09-30-long-session/trial-1-memory-failure.json).

The resulting correction uses the existing history tool and input index:
user-input-only pagination, readable event text by default, raw JSON on explicit
request, and summary instructions that preserve precise unfulfilled commitments
without copying the whole ID index. It adds no memory service or task layer.

A supplementary review also checked raw timestamps with surrounding whitespace,
an edge not covered by the frozen verifier. The generated code accepted and
trimmed them. An explicit followup in the same Session required rejecting that
raw format. On the same first binary, four additional calls (22,194 tokens,
33.00 seconds, unknown cost) produced a correction passing twelve supplementary
checks and the original nine checks. This is reported separately, not used to
change the first run's failed outcome. Evidence is in
[supplemental-review.json](results/2026-09-30-long-session/trial-1-supplemental-review.json)
and [explicit-timestamp-followup.json](results/2026-09-30-long-session/trial-1-explicit-timestamp-followup.json).

Raw output, histories and generated workspaces are retained locally at
`/Users/zzhang/.bone/acceptance/2026-09-30-long-session-ecnv0gdn`.
`strict-timestamp-followup/workspace-before` preserves the original twelve-turn
final files; `trial-1/workspace` includes the subsequent explicit correction.

### Second live run: old queued input and current assignment

The second frozen binary
`f277303015bd9a94e62ad71ef0896abae8836fa609b29b02db4ae9f284a30b9a`
ran the same twelve inputs, model, limits, task and verifier. The complete gate
again **failed**: the first model request failed with an HTTP transport error,
and rounds five through twelve retained one functional defect, missing strict
UTC timestamp validation. The final external result was eight of nine checks.
There were 75 calls, nine summaries and 655.02 seconds of execution. The known
successful calls reported 293,463 tokens; because the first request's usage is
unknown, aggregate token fields and cost remain `null`.

The failed first input remained queued and pinned verbatim: it asked for
inspection without edits. In round five, the model answered that older task
instead of implementing the current UTC requirement. Runtime records identified
the correct current input, but the native user messages did not label their
input IDs or active/queued status. This is an observed task-following failure
with contextual ambiguity, not proof that the scheduler selected the wrong ID.

The correction adds a derived identity text part to each native input, matching
the preamble's active ID and distinguishing queued, historical and shared
inputs. Original message parts and provider fields remain unchanged. A
regression covers the failed read-only input beside the newer edit request.
Round six answered CSV/dry-run correctly, but its original input was still
pinned; that result alone cannot establish recovery from compressed history.

All control and ownership checks, no-write phases and file boundaries passed
apart from the first transport failure. Eight later quality-failure records
refer to the same UTC gap. Evidence:
[result](results/2026-09-30-long-session/trial-2-result.json),
[rounds](results/2026-09-30-long-session/trial-2-rounds.jsonl), and
[independent review](results/2026-09-30-long-session/trial-2-review.json).

### Focused continuation on the original failed Sessions

The third frozen binary
`cfbb414a09e20eb64061629e105cff4164ad9c63ee582b0ba4eb2d7c3277147f`
was tested on the original saved Sessions with the same model and execution
limits. These are explicit continuation checks, not replacement twelve-turn
trials or a controlled efficiency comparison.

| Continuation | Calls | Reported tokens | Seconds | Additional summaries | External result |
| --- | ---: | ---: | ---: | ---: | --- |
| Recover original deferred requirements in trial one | 21 | 88,321 | 120.02 | 2 | CSV/dry-run recovered with valid sources; unchanged files; 9/9 code checks |
| Correct UTC validation in trial two | 8 | 31,647 | 72.64 | 1 | Current edit completed; 9/9 original code checks |

Before the memory check, all Jobs had empty active/inbox slots, the first two
inputs had already delivered, both were absent from the hot history, and twelve
summary events existed. The new prompt supplied neither the CSV nor dry-run
answer. The model used user-input pagination and read the complete first
original input from SQLite. Its six final source IDs and revisions were valid,
and it distinguished the deferred features from details supplied later.
No write action started and all file hashes were unchanged.

This did not show perfect retrieval efficiency: the second input was only
previewed, not read in full, and four incorrectly copied CLI event IDs produced
lookup errors before the model recovered the correct ID. A further summary
occurred before the full original-input read and another after it. The result
demonstrates cold-history recovery, not that summaries retain every detail or
that the model never needs to recover from its own retrieval mistakes.

The UTC correction supplied the concrete independent failures and explicitly
requested implementation, also clarifying that raw timestamp whitespace must
be rejected. The same Session still contained the original queued read-only
input. The new input completed and the original nine external checks passed.
Independent supplementary probes rejected five invalid timestamp forms across
library add/write/read and the CLI (20 checks), preserved the tested data bytes,
and left workspace hashes unchanged. Only `ledger/storage.py` changed during the
model's correction. The probe script remains at `utc-focused/probe.py`; its
[case results](results/2026-09-30-long-session/utc-focused-supplemental-probe.json)
are retained here.

Neither focused result changes the failed status of its original full trial;
this is evidence for continuation and correction, not an autonomous success-rate
claim. Costs remain unknown. The pre-correction trial-two workspace is retained
at `utc-focused/workspace-before` under the local artifact directory.

Compact continuation evidence is retained alongside the original trials:
[memory result](results/2026-09-30-long-session/memory-focused-result.json),
[memory review](results/2026-09-30-long-session/memory-focused-review.json),
[UTC result](results/2026-09-30-long-session/utc-focused-result.json), and
[UTC review](results/2026-09-30-long-session/utc-focused-review.json).

## Validation status

### Long-session hardening (2026-09-30)

Offline gates passed after the hardening, history-retrieval, input-identity and credential-lock changes: 97 Rust checks (including
three compile-fail API boundary checks), five Python harness checks,
`cargo fmt --check`, Clippy with warnings denied, and the all-features compile
check. One ignored Rust entry is a subprocess helper explicitly executed by its
passing hard-kill parent test, not an omitted acceptance scenario. The final
suite includes the independent oversized-context and cross-Job
original-requirement retrieval tests.

After the third binary's live checks, final code review found that changing
`HOME` while pointing `CODEX_HOME` at the same login source could split credential
locks. The existing OS-user-home helper now serves both workspace and credential
lock locations; reuse holds a stable lock plus the current legacy HOME lock,
deduplicating canonical directory aliases. Three synthetic regressions cover
different legacy locations sharing the stable lock, alias deduplication, and
cancelling a new caller while an older caller holds the legacy lock. This final
lock-location correction received offline validation and independent code
review; it made no additional live requests. The third binary's metrics remain
attributed to its recorded hash, not to this later source change.

The working-set stress test seeds 2,200 large native events (10,572,657 bytes of
original event data), then summarizes and reopens four times. Serialized
resident event data stays about 0.91–0.93 MB; the original records remain
retrievable. This measures cached event data, not process RSS or a constant
memory bound. A separate test opens 80 Idle/Closed Jobs with 100 KB source
bodies and retains less than 100 KB of serialized event cache.

Fault checks cover a completion-transaction failure followed by reopening,
legacy unknown-write markers, current and legacy locks held by a foreground
shell after its owner is killed, and rejection of reconciliation or competing
writes until that shell exits. A recovery test interrupts between input
preparation and model start to verify that an older shared correction remains
visible. A natural-pause regression drives the runtime after the next answer
to ensure the old pause cannot trigger again.

Two default 32 KiB file results exceed the 32,000-character request limit in the
oversized-context fixture. It verifies a real summary, native call/result
pairing, actual `job_inspect` pagination, eventual delivery, and both original
SQLite records. The fallback was already implemented when this independent
integration test ran; the integration result is a post-fix validation, not a
recorded pre-fix failure.

### Initial rebuild baseline

Observed offline validation before the long-session hardening:

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
