# BONE real-world evaluation system

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

You need a running Docker daemon, [`uv`/`uvx`](https://docs.astral.sh/uv/), and an OpenAI API key. A ChatGPT subscription login on the host cannot be used by BONE inside Harbor's task containers.

Set the key only in your local shell. Do not paste it into a command argument, manifest, log, issue, or commit:

```bash
export OPENAI_API_KEY='your-key-here'
```

The runner inherits this variable and never prints its value.

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
