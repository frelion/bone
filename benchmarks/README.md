# BONE real-world evaluation system

For local architecture checks using Podman and an existing ChatGPT subscription login, use `python3 -m benchmarks.architecture` (Python 3.11+), documented below. The Harbor/API-key prerequisites in the next sections apply to that separate runner, not to the local architecture checks.

This directory measures whether BONE can complete real software-engineering work in isolated project containers. Terminal-Bench 2 is the primary public benchmark. Each task is scored by its independent verifier, so BONE's own claim that it finished is never treated as success.

## What is pinned

Every suite manifest pins the boundaries needed for a comparable run:

- Harbor `0.23.0`
- dataset `terminal-bench@2.0` (89 tasks in the full suite)
- BONE release `0.3.2`, installed from its checksummed GitHub release asset
- model `openai/gpt-5.6-luna`
- task selection, attempt count, concurrency, and timeout multiplier

The checked-in suites are:

| Suite | Tasks | Attempts | Purpose |
| --- | ---: | ---: | --- |
| `terminal-bench-2-smoke` | 3 fixed | 1 | Fast adapter and end-to-end health check |
| `terminal-bench-2-regression` | 20 fixed | 3 | Routine product comparison and observed pass@3 |
| `terminal-bench-2-full` | all 89 | 1 | Broad release-candidate measurement |

Pull-request CI validates manifests, command generation, the Harbor adapter import, and the result summarizer. It deliberately does not spend model credits or require Docker. Paid benchmark runs are explicit release gates.

## Prerequisites

You need Python 3.11 or newer, a running Docker daemon, [`uv`/`uvx`](https://docs.astral.sh/uv/), and an OpenAI API key. A ChatGPT subscription login on the host cannot be used by BONE inside Harbor's task containers.

Set the key only in your local shell. Do not paste it into a command argument, manifest, log, issue, or commit:

```bash
export OPENAI_API_KEY='your-key-here'
```

Harbor pipes this value to `bone credentials set`, so the Rust credential backend creates the
private file and endpoint fingerprint. It then starts `bone run` with only `BONE_HOME`; that
process does not receive or read the API-key environment variable.

## Validate before spending money

Run contract tests and validate all manifests:

```bash
python3 -m unittest discover -s benchmarks/tests -v
python3 -m benchmarks.suite validate --all
```

Ask the exact pinned Harbor version to resolve the smoke configuration:

```bash
python3 -m benchmarks.suite validate terminal-bench-2-smoke --harbor
```

Check Docker, Harbor, the API-key presence, and the published BONE binary:

```bash
python3 -m benchmarks.suite preflight terminal-bench-2-smoke
```

To inspect the exact command without running it:

```bash
python3 -m benchmarks.suite command terminal-bench-2-smoke
```

## Run the ladder

Start with smoke and do not proceed if the adapter or infrastructure is broken:

```bash
python3 -m benchmarks.suite run terminal-bench-2-smoke
```

After smoke is healthy, run the fixed regression suite:

```bash
python3 -m benchmarks.suite run terminal-bench-2-regression
```

Use the full suite for a release candidate after regression results have been inspected:

```bash
python3 -m benchmarks.suite run terminal-bench-2-full
```

Jobs are stored under `benchmarks/results/`, which is intentionally gitignored. Give a run a stable label when comparing versions:

```bash
python3 -m benchmarks.suite run terminal-bench-2-regression \
  --job-name bone-0.3.2-luna-baseline
```

## Artifacts and summaries

The Harbor adapter retains these BONE artifacts for each trial:

- `bone-result.json`: native headless status, duration, exit code, and changed files
- `bone-trajectory.json`: durable BONE session-event history
- `bone-exit-code.txt`: native process exit code

Harbor additionally writes `result.json` with verifier rewards and `lock.json` with the resolved task/configuration record. Generate the normalized JSON and human-readable report after a completed job:

```bash
python3 -m benchmarks.suite summarize \
  benchmarks/results/bone-0.3.2-luna-baseline \
  --suite terminal-bench-2-regression
```

This writes `bone-summary.json` and `bone-summary.md` beside Harbor's artifacts.

## Metric rules

The report keeps all attempts, including zero rewards and infrastructure errors:

- **All-scheduled pass rate:** passing trials divided by every scheduled trial. Infrastructure failures and scheduled trials with no result remain failures here.
- **Scored-only pass rate:** passing trials divided only by trials that produced a verifier reward. This diagnostic number must never replace the all-scheduled rate.
- **Observed pass@k:** fraction of tasks that passed at least once among the attempts actually present in the job. For the regression manifest, `k=3` only when all three attempts completed.
- **Failure categories:** verifier failure, infrastructure error, and missing verifier result are reported separately.
- **BONE status:** `completed`, `failed`, `needs_input`, timeout, and other native states are counted independently of the verifier reward.

Token and cost totals are included when Harbor receives them from the agent. The custom BONE adapter currently preserves execution artifacts but may not expose complete provider usage, so a missing cost value means “not reported,” not zero.

## Comparison discipline

For every published number, retain the complete job directory and record:

- BONE commit, release version, and release checksum
- exact model ID and provider configuration
- Harbor version, dataset version, task selection, and task digests
- attempt count, concurrency, timeouts, verifier rewards, native BONE status, and trajectories
- every failed or interrupted attempt

Compare changes on the same suite configuration. Use smoke for plumbing, regression for iteration, and the full suite sparingly; repeatedly tuning against the full public set weakens its value as a release check. A separate private held-out set can be added later for contamination-resistant product decisions.

## Local behavior regression

`benchmarks.behavior` checks BONE's execute-and-verify behavior separately from
Terminal-Bench infrastructure. It runs five fixed cases serially in
native-architecture Ubuntu 24.04 Podman containers and uses the existing BONE
ChatGPT subscription cache. Provide Linux binaries for the candidate and,
optionally, the baseline:

```bash
python3 -m benchmarks.behavior \
  --binary target/linux/release/bone \
  --baseline-binary target/linux-baseline/release/bone
```

The headless Job contract can additionally enforce descendant and depth ceilings
with `--job-budget` and `--job-depth`. For architecture trials, opt in with
`python3 -m benchmarks.architecture --binary <candidate> --enforce-job-contract`.
The metadata distinguishes kernel-enforced trials from the default prompt-only
trials; compare prompt-only baseline/candidate runs separately from contract tests.
Use Python 3.11+ explicitly if the system `python3` points to an older interpreter.
To reuse a particular running Podman VM without changing the global default,
set `CONTAINER_CONNECTION` for the runner process.

Candidate cases run three times by default; baseline cases run once. Prompts,
native results, trajectories, verifier logs, and the aggregate `summary.json`
are written below `benchmarks/results/behavior/`. Credentials are mounted from
the private BONE cache and are never copied into that directory. These results
are local behavior regressions, not Terminal-Bench scores.

## Aider Polyglot Python smoke

`benchmarks.polyglot` runs five fixed Python exercises from Aider's public
Polyglot benchmark. The source repository is pinned to an exact commit and
cached below the ignored results directory. Each exercise runs in its own
native-architecture Podman container. Solution files are writable under
`/app`; independent tests are mounted read-only under `/tests`.

```bash
/opt/homebrew/bin/python3 -m benchmarks.polyglot \
  --binary target/behavior-linux/candidate/release/bone
```

The default suite is `proverb`, `grade-school`, `phone-number`, `robot-name`,
and `wordy`, with one attempt per task. Use repeated `--task` options for a
smaller diagnostic run. This is a stable BONE smoke subset, not a claim of
comparability with Aider's full multi-language leaderboard.

Each trial also writes `efficiency-profile.json`. It reconstructs Job ownership,
model and tool call durations, concurrent call activity, time outside calls,
cross-Job duplicate reads, and the delay after the final successful test run.
Profiles contain tool names and outcomes but never copy tool arguments.

## Job architecture experiments

Prerequisites: Python 3.11+, a running Podman machine, the saved BONE ChatGPT
subscription login, and a native Linux BONE binary. Run the small controlled
workloads with the existing Podman image:

```bash
python3 -m benchmarks.architecture \
  --binary target/behavior-linux/candidate/release/bone --attempts 2
```

This runs 14 fresh-container trials: a single-Job normalization task, plus
independent and sequential workloads in single-Job, explicitly delegated, and
autonomous-decomposition modes. Modes share fixtures and an independent verifier;
alternate rounds reverse mode order. Use `--workload independent --mode delegated`
to repeat one condition. Trials run sequentially to avoid cross-trial model load.
`--case independent-auto` selects an exact condition; `--timeout-seconds` defaults
to 120 for these small tasks. Each completed trial saves `trial-result.json`
before container cleanup, so an interrupted cleanup cannot lose its result.
Before cleanup the runner also reads the trial's SQLite Core chunks in read-only
mode and saves only Core records to `core-records.json`, or an explicit
`core-records-error.log` if capture fails. It does not export profiles or the auth
cache. Records can contain fixture contents and model notes, so keep raw trial
artifacts local and review them before sharing. These private records supplement,
but do not change, the public-history efficiency metrics below.

`summary.json` separates task success from compliance with the requested Job
structure. Profiles report child model-call overlap and parent model-call count.
Call durations include provider and network latency; they are not inference-time
measurements. Public history does not expose token usage, retries, or each chosen
WorkStep. Cross-Job duplicate reads cover only the `read` tool. Verification timing
is a heuristic for unittest output, not proof of complete acceptance coverage.
These small workloads measure coordination overhead, not general coding ability
or the usefulness of delegation on larger tasks.

The initial investigation, failed optimization experiments, and architecture
recommendations are recorded in [the study](../docs/job-efficiency-study.md).
The implemented protocol is described in [Job control](../docs/job-control-protocol.md).
The retained and rejected candidates, bounded acceptance results, and limits are
recorded in [the validation report](../docs/job-control-validation.md).
