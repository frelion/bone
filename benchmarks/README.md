# BONE real-world evaluations

This directory runs BONE against independently authored software-engineering
tasks in isolated project containers. Success is decided by each task's hidden
verifier, not by BONE's own completion message.

## Evaluation ladder

1. **Smoke:** 3 fixed tasks, one attempt each. Run before changing prompts,
   tools, orchestration, or model adapters.
2. **Regression:** 20 fixed tasks, three attempts each. Report pass rate,
   pass@3, timeouts, crashes, and `needs_input` separately.
3. **Full:** an immutable public dataset version plus a private held-out set.
   Pin the BONE release, model ID, Harbor version, task digests, and attempt
   count in the result record.

Do not put a paid-model run in ordinary pull-request CI. CI verifies the
headless protocol without credentials; smoke/regression/full runs are explicit
release-gate jobs.

## Prerequisites

- Docker
- [uv](https://docs.astral.sh/uv/)
- A provider API key

```bash
uv tool install harbor
export OPENAI_API_KEY='...'
export PYTHONPATH="$PWD"
```

## Run BONE through Harbor

The custom adapter downloads a checksummed BONE release into every task
container, runs it in `/app`, and retains these artifacts under the Harbor job:

- `bone-result.json` — stable status and changed-file summary
- `bone-trajectory.json` — complete durable BONE session-event history
- `bone-exit-code.txt` — native headless exit code

Start with a single real task and a pinned BONE version:

```bash
harbor run \
  -d terminal-bench/terminal-bench-2 \
  --agent benchmarks.harbor.agent:BoneAgent \
  --agent-kwarg version=0.3.1 \
  --model openai/gpt-5.6-sol \
  --agent-env OPENAI_API_KEY="$OPENAI_API_KEY" \
  --n-tasks 1 \
  --n-attempts 1
```

Use `harbor run --help` to confirm flag names for the installed Harbor version;
Harbor's Python import path is the stable integration boundary, while some CLI
aliases have changed between releases.

Recommended public suites:

```bash
# Broad terminal/software-engineering behavior.
harbor run -d terminal-bench/terminal-bench-2 \
  --agent benchmarks.harbor.agent:BoneAgent \
  --agent-kwarg version=0.3.1 \
  --model openai/gpt-5.6-sol

# Repository issue fixing with independent tests.
harbor run -d swe-bench/swe-bench-verified \
  --agent benchmarks.harbor.agent:BoneAgent \
  --agent-kwarg version=0.3.1 \
  --model openai/gpt-5.6-sol
```

SWE-bench Multilingual should be added as soon as its task package is available
in the chosen Harbor registry. Its Rust slice is the best language-matched
public suite for BONE; keep the selected task IDs fixed across comparisons.

## Direct local scenario

`bone run` can also execute any checked-out project without Harbor:

```bash
export OPENAI_API_KEY='...'
bone run \
  --workspace /path/to/project \
  --prompt-file issue.md \
  --model gpt-5.6-sol \
  --trajectory artifacts/trajectory.json \
  --result artifacts/result.json
```

The JSON printed on stdout is the automation contract. Human progress goes to
stderr. Exit codes are: `0` completed, `2` bad invocation, `3` agent/provider
failure, `4` clarification required, `124` timeout, and `130` interruption.

## Result discipline

For every published number, retain:

- BONE commit and release asset checksum
- model provider, exact model ID, and model parameters
- Harbor version and dataset/task digests
- per-task verifier reward, BONE status, exit code, duration, and trajectory
- all attempts, including infrastructure failures and zero scores

Never tune on the private held-out set. Rotate it periodically and inspect task
contamination before treating a score increase as product progress.
