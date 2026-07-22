# Bone TUI Design Language

## Product context

- Bone is a keyboard-first coding workspace for developers who repeatedly scan conversations, inspect tool work, and write prompts.
- The interface must remain useful from a 60-column remote terminal through a wide desktop terminal, in dark and light themes.
- Information density is compact but not compressed: the transcript and composer are primary; navigation and metadata stay quiet.

## Design direction

- Calm, precise, and work-focused. Bone should feel like a native developer tool, not a dashboard or chat website rendered in a terminal.
- Use one accent for focus and primary state. Reserve success, warning, and danger colors for meaning.
- Prefer alignment, contrast, and whitespace over boxes. Avoid nested frames, decorative gradients, and full-width colored message cards.
- Keep persistent chrome to one line where possible. Secondary detail appears on demand or disappears at narrow widths.

## Layout and density

- At 88 columns and wider, show conversations beside the workspace. The sidebar is 28-34 columns and the transcript measure is capped at 112 columns.
- Below 88 columns, show one full-width pane at a time. Focusing conversations or chat switches the visible pane without changing key bindings.
- Dialogs use a bounded centered surface on wide terminals and a full-width, single-column surface on narrow terminals.
- Conversation rows use at most two visible lines by default. Transcript blocks use one blank row between turns, not padding around every element.

## Visual hierarchy

- Primary: transcript content and the active composer.
- Supporting: current conversation, model, thinking level, working/error state.
- Contextual: conversation previews, timestamps, tool details, extension widgets.
- Optional: help text and advanced metadata. Do not permanently spend rows on shortcut instructions.

## Components and states

- Shell: responsive split/single-pane workspace with stable top and bottom chrome.
- Composer: one quiet input surface, visible focus marker, bounded multiline growth, autocomplete above the input.
- Conversation list: persistent foreground marker, separate keyboard selection, search and destructive confirmation states.
- Transcript: visually distinct user, assistant, tool, plan, summary, error, and streaming states without card nesting.
- Dialog: opaque backdrop, stable title/body/status regions, responsive row composition, and focus restoration.
- Every component owns loading, empty, focused, selected, disabled, error, and destroyed states where applicable.

## Performance and accessibility

- Streaming updates mutate existing text/markdown nodes. They must not rebuild the transcript subtree for every token.
- Resize changes layout in place. Focus, draft text, selection, and transcript scroll position survive a layout-mode change.
- Focus and selection use structure or symbols in addition to color. Semantic status always includes text.
- Destruction is idempotent. No view reads native OpenTUI buffers after renderer teardown.

## Source of truth

- Design intent: this document.
- Responsive metrics and component semantics: `packages/coding-agent/src/modes/interactive/opentui-design.ts`.
- Theme colors: `packages/coding-agent/src/modes/interactive/theme/`.
- Renderer behavior: `packages/tui/src/opentui/`.
- Executable visual contracts: OpenTUI tests in `packages/tui/test/` and `packages/coding-agent/test/`.
