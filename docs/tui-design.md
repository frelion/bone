# Bone TUI Design Language

## Product shape

Bone is a Codex/Claude Code-style conversation workspace with an OpenCode-toned conversation sidebar. It is a keyboard-first coding tool, not a dashboard, article reader, or card-based chat application.

## Visual direction

- Bone owns a dark, opaque surface ladder derived from OpenCode: page, panel, element, border, text, and muted text.
- Warm orange is the sole product accent. Semantic success, warning, and error colors remain distinct.
- The transcript is a continuous terminal flow. Do not label turns with `YOU` or `BONE`, constrain them to an article-width column, or wrap routine content in cards.
- The sidebar is the persistent high-recognition surface. Current and keyboard-selected conversations use separate orange surface tokens in addition to status symbols.
- Use borders and separators only where they explain ownership or interaction. Avoid nested frames, decorative padding, and permanent help text.

## Layout

- The sidebar starts at 38 columns, can be dragged from 32-60 columns, and persists globally.
- Split layout is available only when the selected sidebar width, one separator column, and at least 60 main columns fit.
- When they do not fit, conversations and chat become full-width pages. The existing focus actions switch pages.
- The transcript uses every available main-pane column with one column of terminal padding. It has no maximum reading width.
- The composer is fixed at the bottom. Its context status remains inside the composer border and must not change the composer's outer height while streaming.
- Wide dialogs are centered on an opaque backdrop. Narrow dialogs replace the main pane.

## Conversation sidebar

- The only persistent header content is `CONVERSATIONS`.
- Each conversation has exactly three rows: status and title; live latest-message preview with optional throughput; message count, creation time, and last activity.
- Rows are separated by a subtle rule and have no blank row between them.
- Current conversation uses the strong orange surface. Keyboard selection uses a darker orange surface. These are independent states.
- All overflowing visible previews marquee horizontally. Hidden rows and a hidden sidebar stop animating. Updates are capped at four frames per second.
- Arrow keys move selection, Enter activates, `/` enters the existing search flow, and mouse click activates immediately. Existing delete, preview, background execution, and search contracts remain unchanged.
- Ordering is last-activity descending. While the sidebar is focused, ordering is frozen and reconciled once focus leaves.

## Transcript

- User turns follow Codex prompt treatment without a speaker label. Assistant text flows directly into the transcript without a speaker label or background card.
- Continuous tool executions form a single `Working` activity group. The running group is expanded; successful groups collapse when complete; failed tools remain expanded.
- `Ctrl+O` toggles global tool detail. Clicking a group toggles that group without changing the global mode.
- Thinking appears only as a live working summary and disappears when the run finishes.
- Streaming mutates existing nodes when segment structure is stable.
- Auto-follow operates only while the viewport is at the bottom. Any upward user scroll suspends it until the viewport returns to the bottom.
- Mouse wheel events anywhere in the main conversation area scroll the transcript.

## Composer and empty state

- The composer uses a Codex-style full border and internal padding.
- Its internal status shows cwd, model, thinking level, context remaining, and current foreground throughput.
- The composer does not display `Message Bone`, permanent shortcuts, or status outside its border.
- A new empty conversation shows a one-time compact Codex-style welcome state. It disappears after the first user turn and is not a marketing page.

## State and performance contracts

- Focus, selection, running, waiting, failure, search, deletion confirmation, empty, dialog, dragging, and destroyed states must be visually and behaviorally explicit.
- Focus and state never rely on color alone.
- Resize and sidebar drag mutate layout in place. Draft, cursor, transcript scroll, selection, and extension state survive.
- Marquee, throughput, and spinner updates have bounded cadence and never rebuild the sidebar or transcript tree per token.
- Destruction is idempotent. No component reads native OpenTUI buffers after renderer teardown.

## Source of truth

- Product and design intent: this document.
- Metrics and semantic colors: `packages/coding-agent/src/modes/interactive/opentui-design.ts`.
- Generic content theme: `packages/coding-agent/src/modes/interactive/theme/`.
- Renderer interaction contract: `packages/tui/src/opentui/`.
- Executable visual contracts: OpenTUI tests under `packages/tui/test/` and `packages/coding-agent/test/`.
