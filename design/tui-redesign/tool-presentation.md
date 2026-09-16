# Tool presentation and execution feedback

2026-09-16. Built on baseline `26db99d82`.

Tool modules own the meaning of their recorded arguments and results. `tools::presentation` has one static dispatcher and three small types: `ToolSummary`, `ToolDetails`, and `TextSection`. The application exposes `tool_summary` / `tool_details`, including its own `session_history` tool. No presentation method is added to the execution ports, and no presentation data is persisted or returned to the model as extra history fields.

Public tool history and activity now retain recorded arguments. The core already stores them; the application projection had omitted them. Reading details does not construct a tool instance, access the workspace, or rerun a command.

The transcript uses the existing bounded source cache for tool summaries. Details are built only when opened. The TUI renders text sections, wrapping and optional line-number gutters; it does not interpret tool result JSON. Unknown or unrecognized results retain a generic compact raw-data view. Compact serialization avoids indentation amplification for deeply nested results.

`read.content` is source text, including original line endings and a final newline when present. Line numbers are layout metadata, never part of copied text. The old numbered-string format is removed with no compatibility parser. Clipboard extraction uses the selected source bytes; screen rendering still sanitizes terminal control characters.

Execution feedback is derived from pending submissions, active calls, jobs and inputs. A 120 ms TUI-only tick animates outstanding work. An idle attached runtime does not animate. User waits and pauses are static. Existing call progress is displayed when available; this does not add token streaming or invent percentage progress.

Consecutive ordinary tool and lifecycle events use compact spacing; message and error boundaries retain separation. Returned tool results use a neutral tone because a successful tool transport can still contain a nonzero process exit code. Tool errors remain distinct. Mouse actions and animation do not change keyboard ownership.

Cross-review corrected narrow line-number gutters, CRLF clipboard preservation, nested JSON expansion, duplicate error text, misleading success coloring, and details painting over the default rail placeholder. No provider model settings or host terminal were modified.

Validation: workspace tests; TUI tests and 13 real PTY tests; all-target workspace Clippy with warnings denied; format and diff checks; explicit 1 MiB reader scroll tests. Production-renderer preview scenarios `tool-presentation` and `tool-details` exercise 160×40 and 70×28 layouts. Preview fixtures are synthetic, not live model calls. macOS was not tested on hardware in this Linux/WSL environment.
